//! Windows pseudoconsole ownership. Input, output and teardown can each block independently.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::*;

mod conpty;
pub use conpty::Console;

fn coord(cols: u16, lines: u16) -> COORD {
    COORD {
        X: cols as i16,
        Y: lines as i16,
    }
}

fn hresult(result: i32) -> io::Result<()> {
    if result >= 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result & 0xffff))
    }
}

fn check(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (null_mut(), null_mut());
    check(unsafe { CreatePipe(&mut read, &mut write, null(), 0) })?;
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

pub fn readable(pipe: &File) -> io::Result<bool> {
    let mut count = 0;
    check(unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            &mut count,
            null_mut(),
        )
    })?;
    Ok(count > 0)
}

// Aligned storage stays alive until DeleteProcThreadAttributeList returns.
struct Attributes(Vec<usize>);

impl Attributes {
    fn new(console: &Console) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        check(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 1, 0, &mut bytes)
        })?;
        let mut list = Self(storage);
        check(unsafe {
            UpdateProcThreadAttribute(
                list.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console.handle as _,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        })?;
        Ok(list)
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn wide(s: &OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<_> = s.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::other("路径或参数含空字符"));
    }
    units.push(0);
    Ok(units)
}

// The Windows C runtime doubles trailing backslashes inside quotes and escapes literal quotes.
fn quoted(arg: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        let count = if c == '"' { slashes * 2 + 1 } else { slashes };
        result.extend(std::iter::repeat_n('\\', count));
        result.push(c);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

fn has_persisted_no_color() -> bool {
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    };
    use windows_sys::Win32::System::Registry::*;
    use windows_sys::core::w;
    // A user value overrides the machine value. A size-only query checks for a nonempty
    // string without reading other environment settings. Preserve the preference on errors.
    for (key, path) in [
        (HKEY_CURRENT_USER, w!("Environment")),
        (
            HKEY_LOCAL_MACHINE,
            w!("SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment"),
        ),
    ] {
        let mut size = 0;
        let status = unsafe {
            RegGetValueW(
                key,
                path,
                w!("NO_COLOR"),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
                null_mut(),
                null_mut(),
                &mut size,
            )
        };
        match status {
            ERROR_SUCCESS => return size > 2,
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => (),
            _ => return true,
        }
    }
    false
}

pub(crate) fn terminal_environment(
    vars: &mut Vec<(OsString, OsString)>,
    marker: &str,
    persisted_no_color: impl FnOnce() -> bool,
) {
    let value_is = |name: &str, value: &str| {
        vars.iter()
            .any(|(k, v)| k.eq_ignore_ascii_case(name) && v == value)
    };
    // Codex's noninteractive command runner supplies this pair for its captured output.
    // An independent, interactive PTY has its own capabilities. A user's NO_COLOR still wins.
    let captured_no_color = value_is("TERM", "dumb")
        && value_is("NO_COLOR", "1")
        && value_is("CODEX_CI", "1")
        && value_is("CODEX_NON_INTERACTIVE", "1")
        && !persisted_no_color();
    let owned = [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "Mcli"),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
        ("MCLI_SESSION_ID", marker),
    ];
    vars.retain(|(key, _)| {
        !(owned.iter().any(|(name, _)| key.eq_ignore_ascii_case(name))
            || captured_no_color && key.eq_ignore_ascii_case("NO_COLOR"))
    });
    vars.extend(owned.map(|(name, value)| (name.into(), value.into())));
    // Windows ConPTY strips OSC 7501 query replies on the input pipe, so declare
    // Mcli's OSC 7501 capability unless the user explicitly configured the agent env vars.
    for (name, value) in [("AGY_CLI_OSC7501", "on"), ("PI_PROGRAM_STATUS", "1")] {
        if !vars.iter().any(|(key, _)| key.eq_ignore_ascii_case(name)) {
            vars.push((name.into(), value.into()));
        }
    }
}

fn environment(
    marker: &str,
    proxy_directory: Option<&Path>,
    proxy_catalog: Option<&Path>,
) -> Vec<u16> {
    let mut vars = std::env::vars_os().collect();
    terminal_environment(&mut vars, marker, has_persisted_no_color);
    vars.retain(|(k, _)| {
        !k.eq_ignore_ascii_case("MCLI_STATUS_HELPER")
            && !k.eq_ignore_ascii_case("MCLI_PROXY_DIR")
            && !k.eq_ignore_ascii_case("MCLI_PROXY_CATALOG")
    });
    if let Some(dir) = proxy_directory {
        vars.push(("MCLI_PROXY_DIR".into(), dir.as_os_str().into()));
    }
    if let Some(catalog) = proxy_catalog {
        vars.push(("MCLI_PROXY_CATALOG".into(), catalog.as_os_str().into()));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let helper = dir.join("mcli-hook.exe");
        #[cfg(test)]
        let helper = if helper.is_file() {
            helper
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/mcli-hook.exe")
        };
        if helper.is_file() {
            vars.push(("MCLI_STATUS_HELPER".into(), helper.into_os_string()));
        }
    }
    vars.sort_by_cached_key(|(key, _)| key.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (key, value) in vars {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

pub struct Child {
    pub console: Console,
    pub input: File,
    pub output: File,
    pub process: OwnedHandle,
}

pub fn spawn(
    program: &Path,
    args: &[String],
    cwd: &Path,
    cols: u16,
    lines: u16,
    marker: &str,
    proxy_directory: Option<&Path>,
    proxy_catalog: Option<&Path>,
) -> io::Result<Child> {
    if !cwd.is_dir() {
        return Err(io::Error::other(format!(
            "项目目录不存在：{}",
            cwd.display()
        )));
    }
    let exe = wide(program.as_os_str())?;
    let directory = wide(cwd.as_os_str())?;
    let mut cmd = OsString::from("\"");
    cmd.push(program);
    cmd.push("\"");
    for arg in args {
        cmd.push(" ");
        cmd.push(quoted(arg));
    }
    let mut command_line = wide(&cmd)?;
    let env = environment(marker, proxy_directory, proxy_catalog);
    let (console_in, input) = pipe()?;
    let (output, console_out) = pipe()?;
    let console = Console::create(coord(cols, lines), &console_in, &console_out)?;
    drop((console_in, console_out));
    let mut attrs = Attributes::new(&console)?;
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // A launch from Windows Terminal or a test runner can carry redirected standard handles.
    // Request fresh console handles so the child binds all three streams to this pseudoconsole.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attrs.ptr();
    let mut info = unsafe { zeroed::<PROCESS_INFORMATION>() };
    check(unsafe {
        CreateProcessW(
            exe.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            env.as_ptr().cast(),
            directory.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        )
    })?;
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess),
            OwnedHandle::from_raw_handle(info.hThread),
        )
    };
    drop(thread);
    Ok(Child {
        console,
        input: input.into(),
        output: output.into(),
        process,
    })
}

pub fn stop_event() -> io::Result<OwnedHandle> {
    let h = unsafe { CreateEventW(null(), 1, 0, null()) };
    if h.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(h) })
    }
}

pub fn open_signal(name: &str) -> io::Result<OwnedHandle> {
    let name = wide(OsStr::new(name))?;
    let h = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
    if h.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(h) })
    }
}

pub fn signal_event(event: &OwnedHandle) {
    unsafe { SetEvent(event.as_raw_handle()) };
}

// An explicit terminal close can race the initial shell startup. Give the owned
// process time to handle CTRL_CLOSE_EVENT before ensuring it exits.
pub fn finish_requested_close(process: &OwnedHandle, grace_ms: u32) -> io::Result<()> {
    let handle = process.as_raw_handle();
    match unsafe { WaitForSingleObject(handle, grace_ms) } {
        WAIT_OBJECT_0 => return Ok(()),
        WAIT_TIMEOUT => (),
        _ => return Err(io::Error::last_os_error()),
    }
    if let Err(e) = check(unsafe { TerminateProcess(handle, 1) }) {
        // The process can exit on its own between the timeout and termination.
        if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
            return Err(e);
        }
    }
    if unsafe { WaitForSingleObject(handle, INFINITE) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn wait(process: &OwnedHandle, stop: &OwnedHandle) -> io::Result<Option<u32>> {
    let handles: [HANDLE; 2] = [process.as_raw_handle(), stop.as_raw_handle()];
    match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) } {
        WAIT_OBJECT_0 => {
            let mut code = 0;
            check(unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) })?;
            Ok(Some(code))
        }
        n if n == WAIT_OBJECT_0 + 1 => Ok(None),
        _ => Err(io::Error::last_os_error()),
    }
}
