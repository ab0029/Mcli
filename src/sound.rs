use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundMode {
    Off,
    #[default]
    Background,
    Always,
}

impl SoundMode {
    pub const ALL: [(Self, &'static str, &'static str); 3] = [
        (Self::Off, "off", "关闭"),
        (Self::Background, "background", "仅后台/非当前会话"),
        (Self::Always, "always", "总是播放"),
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundPreset {
    #[default]
    Terminal,
    Notify,
    Message,
}

impl SoundPreset {
    pub const ALL: [(Self, &'static str, &'static str); 3] = [
        (Self::Terminal, "terminal", "Terminal 同款"),
        (Self::Notify, "notify", "系统通知"),
        (Self::Message, "message", "消息提示"),
    ];

    /// Ordered Windows sound event aliases to try before gracefully falling back.
    pub fn alias_chain(self) -> &'static [&'static str] {
        match self {
            Self::Terminal => &["SystemNotification", "SystemAsterisk", ".Default"],
            Self::Notify => &["Notification.Default", "SystemNotification", ".Default"],
            Self::Message => &["Notification.IM", "Notification.Default", ".Default"],
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundPrefs {
    #[serde(default)]
    pub mode: SoundMode,
    #[serde(default)]
    pub preset: SoundPreset,
    #[serde(default = "default_true")]
    pub on_waiting: bool,
}

impl Default for SoundPrefs {
    fn default() -> Self {
        Self {
            mode: SoundMode::Background,
            preset: SoundPreset::Terminal,
            on_waiting: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyNotification {
    Completed,
    Waiting,
}

impl SoundPrefs {
    pub fn should_play(
        &self,
        kind: ReplyNotification,
        window_active: bool,
        tab_selected: bool,
    ) -> bool {
        if kind == ReplyNotification::Waiting && !self.on_waiting {
            return false;
        }
        match self.mode {
            SoundMode::Off => false,
            SoundMode::Always => true,
            SoundMode::Background => !window_active || !tab_selected,
        }
    }
}

static LAST_PLAYED: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

fn claim_debounce() -> bool {
    let lock = LAST_PLAYED.get_or_init(|| Mutex::new(None));
    let Ok(mut guard) = lock.lock() else {
        return false;
    };
    let now = Instant::now();
    if guard.is_some_and(|prev| now.saturating_duration_since(prev) < DEBOUNCE) {
        return false;
    }
    *guard = Some(now);
    true
}

pub fn play_debounced(preset: SoundPreset) {
    if claim_debounce() {
        dispatch_play(preset);
    }
}

pub fn preview(preset: SoundPreset) {
    dispatch_play(preset);
}

#[cfg(test)]
static TEST_EVENTS: OnceLock<Mutex<Vec<SoundPreset>>> = OnceLock::new();

#[cfg(test)]
pub fn take_test_events() -> Vec<SoundPreset> {
    if let Some(lock) = LAST_PLAYED.get()
        && let Ok(mut guard) = lock.lock()
    {
        *guard = None;
    }
    let lock = TEST_EVENTS.get_or_init(|| Mutex::new(Vec::new()));
    std::mem::take(&mut *lock.lock().unwrap())
}

#[cfg(test)]
fn dispatch_play(preset: SoundPreset) {
    let lock = TEST_EVENTS.get_or_init(|| Mutex::new(Vec::new()));
    lock.lock().unwrap().push(preset);
}

#[cfg(not(test))]
fn dispatch_play(preset: SoundPreset) {
    #[cfg(windows)]
    {
        static WORKER: OnceLock<std::sync::mpsc::SyncSender<SoundPreset>> = OnceLock::new();
        let tx = WORKER.get_or_init(|| {
            let (send, recv) = std::sync::mpsc::sync_channel::<SoundPreset>(1);
            let _ = std::thread::Builder::new()
                .name("mcli-sound".into())
                .spawn(move || {
                    while let Ok(preset) = recv.recv() {
                        win_audio::play_safe(preset);
                    }
                });
            send
        });
        // Non-blocking submit so a stalled audio driver can never stall the UI thread.
        let _ = tx.try_send(preset);
    }
    #[cfg(not(windows))]
    let _ = preset;
}

#[cfg(all(windows, not(test)))]
mod win_audio {
    use super::SoundPreset;
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::sync::OnceLock;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

    const SND_ASYNC: u32 = 0x0001;
    const SND_NODEFAULT: u32 = 0x0002;
    const SND_ALIAS: u32 = 0x0001_0000;
    const MB_OK: u32 = 0x0000_0000;

    type PlaySoundWFn = unsafe extern "system" fn(*const u16, *mut c_void, u32) -> i32;
    type MessageBeepFn = unsafe extern "system" fn(u32) -> i32;

    struct AudioProcs {
        play_sound: Option<PlaySoundWFn>,
        message_beep: Option<MessageBeepFn>,
    }

    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn procs() -> &'static AudioProcs {
        static PROCS: OnceLock<AudioProcs> = OnceLock::new();
        PROCS.get_or_init(|| {
            // Load winmm.dll / user32.dll dynamically so stripped Windows installations
            // without multimedia DLLs never fail to launch or crash.
            let play_sound = unsafe {
                let dll = wide_z("winmm.dll");
                let hmod = LoadLibraryW(dll.as_ptr());
                if hmod.is_null() {
                    None
                } else {
                    GetProcAddress(hmod, b"PlaySoundW\0".as_ptr())
                        .map(|p| std::mem::transmute::<_, PlaySoundWFn>(p))
                }
            };
            let message_beep = unsafe {
                let dll = wide_z("user32.dll");
                let hmod = LoadLibraryW(dll.as_ptr());
                if hmod.is_null() {
                    None
                } else {
                    GetProcAddress(hmod, b"MessageBeep\0".as_ptr())
                        .map(|p| std::mem::transmute::<_, MessageBeepFn>(p))
                }
            };
            AudioProcs {
                play_sound,
                message_beep,
            }
        })
    }

    /// Returns true if the user explicitly configured Windows Sound Scheme to "No Sounds" (`.None`).
    fn is_system_sound_scheme_none() -> bool {
        let subkey = wide_z(r"AppEvents\Schemes");
        let mut buf = [0u16; 64];
        let mut bytes = (buf.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                null(),
                RRF_RT_REG_SZ,
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != 0 || bytes < 2 {
            return false;
        }
        let len = (bytes as usize / 2).saturating_sub(1).min(buf.len());
        String::from_utf16_lossy(&buf[..len])
            .trim()
            .eq_ignore_ascii_case(".None")
    }

    pub fn play_safe(preset: SoundPreset) {
        if is_system_sound_scheme_none() {
            return;
        }
        let procs = procs();
        if let Some(play_sound) = procs.play_sound {
            for alias in preset.alias_chain() {
                let wide = wide_z(alias);
                let ok = unsafe {
                    play_sound(
                        wide.as_ptr(),
                        null_mut(),
                        SND_ALIAS | SND_ASYNC | SND_NODEFAULT,
                    )
                };
                if ok != 0 {
                    return;
                }
            }
        }
        if let Some(beep) = procs.message_beep {
            unsafe {
                let _ = beep(MB_OK);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_prefs_respects_mode_foreground_and_waiting_flags() {
        let prefs = SoundPrefs::default();
        assert_eq!(prefs.mode, SoundMode::Background);
        assert_eq!(prefs.preset, SoundPreset::Terminal);
        assert!(prefs.on_waiting);

        // Background mode: active window + selected tab stays silent; unfocused or other tab plays.
        assert!(!prefs.should_play(ReplyNotification::Completed, true, true));
        assert!(prefs.should_play(ReplyNotification::Completed, true, false));
        assert!(prefs.should_play(ReplyNotification::Completed, false, true));
        assert!(prefs.should_play(ReplyNotification::Waiting, false, true));

        let mut no_wait = prefs.clone();
        no_wait.on_waiting = false;
        assert!(!no_wait.should_play(ReplyNotification::Waiting, false, false));
        assert!(no_wait.should_play(ReplyNotification::Completed, false, false));

        let always = SoundPrefs {
            mode: SoundMode::Always,
            ..prefs.clone()
        };
        assert!(always.should_play(ReplyNotification::Completed, true, true));

        let off = SoundPrefs {
            mode: SoundMode::Off,
            ..prefs
        };
        assert!(!off.should_play(ReplyNotification::Completed, false, false));

        for (preset, _, _) in SoundPreset::ALL {
            let chain = preset.alias_chain();
            assert!(!chain.is_empty());
            assert_eq!(chain.last().copied(), Some(".Default"));
        }
    }
}
