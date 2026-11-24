// A vendor hook inherits redirected standard handles. Avoid opening a console window.
#![windows_subsystem = "windows"]

#[allow(dead_code)]
#[path = "../hook_wire.rs"]
mod hook_wire;

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
use windows_sys::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};

fn forward(provider: &str, event: &str, input: &[u8]) -> Option<()> {
    let marker = std::env::var("MCLI_SESSION_ID").ok()?;
    let run = std::env::var("MCLI_LAUNCH_ID").ok()?;
    let (pid, name) = hook_wire::host(&marker)?;
    let name = hook_wire::pipe_name(&name);
    let wide: Vec<_> = name.encode_utf16().chain([0]).collect();
    let open = || {
        OpenOptions::new()
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION | SECURITY_SQOS_PRESENT)
            .open(&name)
    };
    let mut pipe = match open() {
        Ok(pipe) => pipe,
        Err(e) if e.raw_os_error() == Some(231) => {
            if unsafe { WaitNamedPipeW(wide.as_ptr(), 50) } == 0 {
                return None;
            }
            open().ok()?
        }
        Err(_) => return None,
    };
    let mut server = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut server) } == 0
        || server != pid
    {
        return None;
    }
    let hook = serde_json::from_slice(input).ok()?;
    let packet = hook_wire::Notice::from_hook(provider, event, marker, run, &hook)?.encode();
    if packet.len() > hook_wire::MAX_PACKET {
        return None;
    }
    pipe.write_all(&packet).ok()
}

fn main() {
    let mut args = std::env::args().skip(1);
    if let (Some(provider), Some(event)) = (args.next(), args.next()) {
        const LIMIT: u64 = 4 * 1024 * 1024;
        let mut input = Vec::new();
        if std::io::stdin()
            .lock()
            .take(LIMIT + 1)
            .read_to_end(&mut input)
            .is_ok()
            && input.len() as u64 <= LIMIT
        {
            let _ = forward(&provider, &event, &input);
        }
        if provider == "agy" && event == "StatusLine" {
            if let Ok(command) = std::env::var("MCLI_AGY_ORIGINAL_STATUS")
                && !command.is_empty()
            {
                use std::os::windows::process::CommandExt;
                use std::process::{Command, Stdio};
                let Some(system) = std::env::var_os("SystemRoot") else {
                    return;
                };
                let shell = std::path::PathBuf::from(system)
                    .join("System32")
                    .join("cmd.exe");
                if let Ok(mut child) = Command::new(shell)
                    .args(["/d", "/s", "/c"])
                    .raw_arg(format!("\"{command}\""))
                    .creation_flags(0x08000000)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                {
                    if let Some(mut target) = child.stdin.take() {
                        let _ = target.write_all(&input);
                    }
                    std::process::exit(child.wait().ok().and_then(|s| s.code()).unwrap_or(1));
                }
                std::process::exit(1);
            }
        } else if provider == "agy" {
            let _ = std::io::stdout().write_all(b"{}\n");
        }
    }
}
