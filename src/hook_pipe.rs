use crate::hook_wire::{self, MAX_PACKET, Notice};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;

fn owned(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

fn pipe_for_user(name: &str, stream: bool) -> io::Result<OwnedHandle> {
    unsafe {
        let mut raw = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = owned(raw)?;
        let mut size = 0;
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size);
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            size,
            &mut size,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut raw_sid = null_mut();
        if ConvertSidToStringSidW(
            (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid,
            &mut raw_sid,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut n = 0;
        while *raw_sid.add(n) != 0 {
            n += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(raw_sid, n));
        LocalFree(raw_sid.cast());
        let sddl: Vec<_> = format!("D:P(A;;GA;;;{sid})")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let wide: Vec<_> = name.encode_utf16().chain([0]).collect();
        let result = owned(CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_REJECT_REMOTE_CLIENTS
                | if stream {
                    PIPE_TYPE_BYTE
                } else {
                    PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE
                },
            1,
            0,
            MAX_PACKET as u32,
            50,
            &attributes,
        ));
        LocalFree(descriptor);
        result
    }
}

// Complete or cancel an in-flight operation before its buffer and OVERLAPPED go out of scope.
fn complete(pipe: HANDLE, stop: HANDLE, op: &mut OVERLAPPED, timeout: u32) -> Option<u32> {
    unsafe {
        let events = [stop, op.hEvent];
        let signalled = WaitForMultipleObjects(2, events.as_ptr(), 0, timeout) == WAIT_OBJECT_0 + 1;
        if !signalled {
            CancelIoEx(pipe, op);
        }
        let mut count = 0;
        let ok = GetOverlappedResult(pipe, op, &mut count, 1);
        (signalled && ok != 0).then_some(count)
    }
}

pub struct Server {
    stop: Arc<OwnedHandle>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(host: &str, receive: impl Fn(Notice) + Send + 'static) -> io::Result<Self> {
        Self::listen(&hook_wire::pipe_name(host), false, move |bytes| {
            if let Some(notice) = Notice::decode(bytes) {
                receive(notice);
            }
        })
    }

    pub fn agy_stream(
        marker: &str,
        run: &str,
        receive: impl Fn(Option<String>) + Send + 'static,
    ) -> io::Result<Self> {
        let suffix = format!("{marker}-{run}");
        let event: Vec<_> = format!(r"Local\{suffix}-ready")
            .encode_utf16()
            .chain([0])
            .collect();
        let ready = owned(unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, event.as_ptr()) }).ok();
        let mut pending = Vec::new();
        let mut oversized = false;
        let server = Self::listen(&format!(r"\\.\pipe\{suffix}-agy"), true, move |bytes| {
            for &byte in bytes {
                if byte == b'\n' {
                    if !oversized
                        && let Ok(line) = std::str::from_utf8(&pending)
                        && let Some((_, id)) =
                            line.rsplit_once("Starting conversation update stream for ")
                        && hook_wire::valid_id(id.trim())
                    {
                        receive(Some(id.trim().to_ascii_lowercase()));
                    } else if !oversized
                        && pending.ends_with(b"Starting new conversation (agent=false)")
                    {
                        receive(None);
                    }
                    pending.clear();
                    oversized = false;
                } else if pending.len() < 2048 {
                    pending.push(byte);
                } else {
                    oversized = true;
                }
            }
        })?;
        if let Some(ready) = ready
            && unsafe { SetEvent(ready.as_raw_handle()) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(server)
    }

    fn listen(
        name: &str,
        stream: bool,
        mut receive: impl FnMut(&[u8]) + Send + 'static,
    ) -> io::Result<Self> {
        let pipe = pipe_for_user(name, stream)?;
        let stop = Arc::new(owned(unsafe { CreateEventW(null(), 1, 0, null()) })?);
        let done = Arc::clone(&stop);
        let event = owned(unsafe { CreateEventW(null(), 1, 0, null()) })?;
        let thread = std::thread::Builder::new()
            .name("mcli-hooks".into())
            .spawn(move || unsafe {
                let pipe = pipe.as_raw_handle();
                let stop = done.as_raw_handle();
                let mut buf = vec![0u8; MAX_PACKET];
                while WaitForSingleObject(stop, 0) == WAIT_TIMEOUT {
                    let mut op = OVERLAPPED {
                        hEvent: event.as_raw_handle(),
                        ..Default::default()
                    };
                    ResetEvent(op.hEvent);
                    let connected = ConnectNamedPipe(pipe, &mut op) != 0
                        || match GetLastError() {
                            ERROR_PIPE_CONNECTED => true,
                            ERROR_IO_PENDING => complete(pipe, stop, &mut op, INFINITE).is_some(),
                            _ => false,
                        };
                    while connected && WaitForSingleObject(stop, 0) == WAIT_TIMEOUT {
                        let mut read = OVERLAPPED {
                            hEvent: event.as_raw_handle(),
                            ..Default::default()
                        };
                        ResetEvent(read.hEvent);
                        let mut n = 0;
                        let received = if ReadFile(
                            pipe,
                            buf.as_mut_ptr(),
                            buf.len() as u32,
                            &mut n,
                            &mut read,
                        ) != 0
                        {
                            Some(n)
                        } else if GetLastError() == ERROR_IO_PENDING {
                            complete(pipe, stop, &mut read, if stream { INFINITE } else { 500 })
                        } else {
                            None
                        };
                        let Some(n) = received.filter(|n| *n > 0) else {
                            break;
                        };
                        receive(&buf[..n as usize]);
                        if !stream {
                            break;
                        }
                    }
                    DisconnectNamedPipe(pipe);
                    if stream {
                        break;
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        unsafe {
            SetEvent(self.stop.as_raw_handle());
        }
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}
