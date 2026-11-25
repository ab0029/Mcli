#![cfg(windows)]
#![cfg_attr(test, allow(dead_code))]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null;
use std::thread::{self, JoinHandle};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, EnumWindows, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

const RELEASE_MUTEX_NAME: &str = r"Local\Mcli-SingleInstance-Mutex";
const RELEASE_ACTIVATE_EVENT_NAME: &str = r"Local\Mcli-SingleInstance-Activate";
const DEV_MUTEX_NAME: &str = r"Local\Mcli-Dev-SingleInstance-Mutex";
const DEV_ACTIVATE_EVENT_NAME: &str = r"Local\Mcli-Dev-SingleInstance-Activate";

fn is_dev_exe_path(path: &std::path::Path) -> bool {
    path.parent()
        .and_then(|d| d.file_name())
        .is_some_and(|n| n.eq_ignore_ascii_case("dev"))
}

/// Returns `true` when launched from a `dev` directory (e.g. `target/dev/mcli.exe`)
/// or when `MCLI_CHANNEL=dev` is explicitly set. Always `false` in unit tests unless overridden.
pub fn is_dev_channel() -> bool {
    #[cfg(test)]
    {
        false
    }
    #[cfg(not(test))]
    {
        if std::env::var_os("MCLI_CHANNEL").is_some_and(|v| v.eq_ignore_ascii_case("dev")) {
            return true;
        }
        std::env::current_exe()
            .ok()
            .is_some_and(|p| is_dev_exe_path(&p))
    }
}

pub fn data_folder_name() -> &'static str {
    if is_dev_channel() { "Mcli-dev" } else { "Mcli" }
}

pub fn app_display_name() -> &'static str {
    if is_dev_channel() {
        "Mcli [DEV]"
    } else {
        "Mcli"
    }
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

pub struct SingleInstance {
    mutex: usize,
    activate_event: usize,
}

pub struct SingleInstanceGuard {
    mutex: usize,
    activate_event: usize,
    stop_event: usize,
    thread: Option<JoinHandle<()>>,
}

impl SingleInstance {
    /// Ensures only one `mcli.exe` runs per user session.
    /// Returns `None` if an instance is already running (after signaling it to surface its window).
    pub fn acquire() -> Option<Self> {
        if is_dev_channel() {
            Self::acquire_named(DEV_MUTEX_NAME, DEV_ACTIVATE_EVENT_NAME)
        } else {
            Self::acquire_named(RELEASE_MUTEX_NAME, RELEASE_ACTIVATE_EVENT_NAME)
        }
    }

    fn acquire_named(mutex_name: &str, event_name: &str) -> Option<Self> {
        let mutex_w = wide(mutex_name);
        let mutex = unsafe { CreateMutexW(null(), 0, mutex_w.as_ptr()) };
        if mutex.is_null() {
            // If mutex creation unexpectedly fails, allow launch rather than blocking the user.
            return Some(Self {
                mutex: 0,
                activate_event: 0,
            });
        }
        let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already {
            unsafe {
                AllowSetForegroundWindow(ASFW_ANY);
                let ev_w = wide(event_name);
                let ev = OpenEventW(EVENT_MODIFY_STATE, 0, ev_w.as_ptr());
                if !ev.is_null() {
                    SetEvent(ev);
                    CloseHandle(ev);
                }
                CloseHandle(mutex);
            }
            return None;
        }
        let ev_w = wide(event_name);
        let activate_event = unsafe { CreateEventW(null(), 0, 0, ev_w.as_ptr()) };
        Some(Self {
            mutex: mutex as usize,
            activate_event: activate_event as usize,
        })
    }

    pub fn listen(mut self, on_activate: impl Fn() + Send + 'static) -> SingleInstanceGuard {
        let mutex = self.mutex;
        let activate_event = self.activate_event;
        self.mutex = 0;
        self.activate_event = 0;

        if activate_event == 0 {
            return SingleInstanceGuard {
                mutex,
                activate_event: 0,
                stop_event: 0,
                thread: None,
            };
        }

        let stop_event = unsafe { CreateEventW(null(), 1, 0, null()) } as usize;
        let thread = if stop_event != 0 {
            thread::Builder::new()
                .name("mcli-single-instance".into())
                .spawn(move || {
                    let handles: [HANDLE; 2] = [stop_event as HANDLE, activate_event as HANDLE];
                    loop {
                        let wait =
                            unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                        if wait == WAIT_OBJECT_0 + 1 {
                            on_activate();
                        } else {
                            break;
                        }
                    }
                })
                .ok()
        } else {
            None
        };

        SingleInstanceGuard {
            mutex,
            activate_event,
            stop_event,
            thread,
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            if self.activate_event != 0 {
                CloseHandle(self.activate_event as HANDLE);
            }
            if self.mutex != 0 {
                CloseHandle(self.mutex as HANDLE);
            }
        }
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if self.stop_event != 0 {
            unsafe {
                SetEvent(self.stop_event as HANDLE);
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            if self.stop_event != 0 {
                CloseHandle(self.stop_event as HANDLE);
            }
            if self.activate_event != 0 {
                CloseHandle(self.activate_event as HANDLE);
            }
            if self.mutex != 0 {
                CloseHandle(self.mutex as HANDLE);
            }
        }
    }
}

unsafe extern "system" fn enum_restore_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    let target_pid = lparam as u32;
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == target_pid && IsWindowVisible(hwnd) != 0 {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
            return 0;
        }
    }
    1
}

/// Restores (if minimized) and focuses the visible top-level window owned by this process.
pub fn bring_current_process_window_to_front() {
    let pid = std::process::id();
    unsafe {
        EnumWindows(Some(enum_restore_proc), pid as LPARAM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    #[test]
    fn second_launch_signals_first_instance_and_releases_on_drop() {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            crate::clawbot::store::now_epoch_ms()
        );
        let mutex = format!(r"Local\Mcli-Test-Mutex-{suffix}");
        let event = format!(r"Local\Mcli-Test-Activate-{suffix}");

        let first = SingleInstance::acquire_named(&mutex, &event).expect("first acquires");
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let guard = first.listen(move || {
            hits_clone.fetch_add(1, Ordering::AcqRel);
        });

        assert!(
            SingleInstance::acquire_named(&mutex, &event).is_none(),
            "second launch must yield to existing instance"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while hits.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(hits.load(Ordering::Acquire), 1);

        drop(guard);
        let third = SingleInstance::acquire_named(&mutex, &event);
        assert!(third.is_some(), "mutex must be released after exit");
    }

    #[test]
    fn dev_exe_path_is_isolated_from_release_channel() {
        assert!(is_dev_exe_path(std::path::Path::new(
            r"L:\AI\Mcli\target\dev\mcli.exe"
        )));
        assert!(is_dev_exe_path(std::path::Path::new(
            r"L:\AI\Mcli\target\DEV\mcli.exe"
        )));
        assert!(!is_dev_exe_path(std::path::Path::new(
            r"L:\AI\Mcli\target\release\mcli.exe"
        )));
        assert_ne!(RELEASE_MUTEX_NAME, DEV_MUTEX_NAME);
        assert_ne!(RELEASE_ACTIVATE_EVENT_NAME, DEV_ACTIVATE_EVENT_NAME);
    }
}
