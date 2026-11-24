use std::io;
use std::path::{Path, PathBuf};

pub fn starting_directory() -> io::Result<PathBuf> {
    let current = std::env::current_dir()?;
    // Explorer usually starts the program beside its executable.
    if std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
        == Some(&current)
        && let Some(home) = std::env::home_dir().filter(|path| path.is_dir())
    {
        return Ok(home);
    }
    Ok(current)
}

pub fn program() -> io::Result<PathBuf> {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    find_program(
        &paths,
        std::env::var_os("ProgramFiles").as_deref().map(Path::new),
        std::env::var_os("SystemRoot").as_deref().map(Path::new),
    )
}

pub(super) fn find_program(
    paths: &std::ffi::OsStr,
    program_files: Option<&Path>,
    system_root: Option<&Path>,
) -> io::Result<PathBuf> {
    std::env::split_paths(paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("pwsh.exe"))
        .chain(program_files.map(|dir| dir.join("PowerShell/7/pwsh.exe")))
        .chain(system_root.map(|dir| dir.join("System32/WindowsPowerShell/v1.0/powershell.exe")))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "找不到 PowerShell，请检查 PowerShell 的安装位置或 PATH",
            )
        })
}

pub fn arguments() -> Vec<String> {
    [
        "-NoExit",
        "-Command",
        include_str!("shell_integration.ps1").trim_start_matches('\u{feff}'),
    ]
    .map(str::to_owned)
    .into()
}

#[cfg(test)]
pub fn arguments_for(resume: Option<&crate::transcript::Key>) -> Vec<String> {
    session_arguments(resume, false)
}

pub fn session_arguments(
    resume: Option<&crate::transcript::Key>,
    track_proxy: bool,
) -> Vec<String> {
    let mut args = arguments();
    let script = args.last_mut().unwrap();
    if track_proxy {
        script.push('\n');
        script.push_str(include_str!("shell_proxy.ps1").trim_start_matches('\u{feff}'));
    }
    if let Some(key) = resume
        && crate::hook_wire::valid_session_id(&key.agent, &key.id)
        && matches!(key.agent.as_str(), "codex" | "agy" | "kimi" | "grok")
    {
        script.push_str(include_str!("shell_restore.ps1").trim_start_matches('\u{feff}'));
        script.push_str(&format!(
            "\n__McliRestore '{}' '{}'",
            key.agent,
            key.resume()
        ));
    }
    args
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Foreground {
    Codex,
    Agy,
    Kimi,
    Grok,
}
impl Foreground {
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Agy => "agy",
            Self::Kimi => "kimi",
            Self::Grok => "grok",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreEnd {
    Finished,
    Unavailable,
    Exhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusSource {
    Osc7501,
    Osc9Ready,
}

pub enum Report {
    Directory(PathBuf),
    Foreground(Option<Foreground>),
    Launch(Foreground, String, Option<String>),
    PrepareAgy(String),
    RestoreStarted,
    RestoreEnded(RestoreEnd),
    StatusQuery(bool),
    Status(Option<Foreground>, &'static str, StatusSource),
    ChildStatus(Option<Foreground>, String, &'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgyEvent {
    Thread(Option<String>),
    Status(&'static str),
    ProgramStatus(&'static str),
    ComposerIdle,
}

// Codex 0.171 truncates "thread-id" to 32 columns. An unnamed "thread-title"
// contains the full ID. A named thread adds a separate name field to the title.
pub fn codex_title_parts(title: &str) -> Option<(&str, &str)> {
    let (short, rest) = title.split_once(" | ")?;
    let prefix = short.strip_suffix("...")?;
    if prefix.len() != 29 || !crate::hook_wire::valid_id(&format!("{prefix}0000000")) {
        return None;
    }
    Some((prefix, rest))
}
pub fn codex_display(title: &str) -> Option<(String, Option<String>)> {
    let (prefix, rest) = codex_title_parts(title)?;
    if crate::hook_wire::valid_id(rest) && rest.starts_with(prefix) {
        return Some((prefix.into(), Some(rest.into())));
    }
    // The native title has a total length limit. Its repeated name can end midway.
    rest.match_indices(" | ")
        .any(|(at, sep)| {
            let tail = &rest[at + sep.len()..];
            !tail.is_empty() && rest[..at].starts_with(tail)
        })
        .then(|| (prefix.into(), None))
}

// Grok Build (`xai-grok-pager/src/notifications/title.rs`) composes OSC 0 titles from
// " - "-separated items including spinner braille, activity labels, and "⚠ Action Required".
fn strip_grok_spinner(s: &str) -> (bool, &str) {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if ('\u{2800}'..='\u{28ff}').contains(&c) => {
            let rest = chars.as_str();
            if rest.is_empty() || rest.starts_with(' ') {
                (true, rest.trim_start())
            } else {
                (false, s)
            }
        }
        _ => (false, s),
    }
}

fn is_grok_activity(s: &str) -> bool {
    matches!(
        s,
        "Thinking" | "Responding" | "Waiting" | "Compacting" | "Running tool"
    ) || s.starts_with("Running: ")
        || s.starts_with("Retrying")
        || s.starts_with("Waiting for ")
}

pub fn grok_title_state(title: &str) -> Option<&'static str> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(" - ").map(str::trim).collect();
    if parts
        .iter()
        .any(|p| p.contains("Action Required") || p.starts_with('\u{26a0}'))
    {
        return Some("等待用户");
    }
    if parts.iter().any(|p| {
        let (has_spinner, rest) = strip_grok_spinner(p);
        has_spinner || is_grok_activity(rest)
    }) {
        return Some("运行中");
    }
    if parts.iter().any(|p| p.eq_ignore_ascii_case("grok")) {
        return Some("就绪");
    }
    None
}

pub fn grok_title_name(title: &str) -> &str {
    for part in title.split(" - ").map(str::trim) {
        let (has_spinner, rest) = strip_grok_spinner(part);
        if part.is_empty()
            || part.eq_ignore_ascii_case("grok")
            || part.contains("Action Required")
            || part.starts_with('\u{26a0}')
            || is_grok_activity(rest)
            || (has_spinner && rest.is_empty())
            || (part.ends_with('s')
                && part.len() >= 2
                && part[..part.len() - 1].bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        return if has_spinner { rest } else { part };
    }
    ""
}

// Observe native reports without consuming any bytes from Alacritty.
#[derive(Default)]
pub struct ShellReports {
    matched: usize,
    path: Vec<u8>,
    waiting_for_st: bool,
    kind: u8,
}

impl ShellReports {
    pub fn feed(&mut self, mut bytes: &[u8], marker: &str, mut report: impl FnMut(Report)) {
        const NOTIFICATION: &[u8] = b"\x1b]9;";
        const AGENT: &[u8] = b"\x1b]633;P;McliAgent=";
        const STATUS: &[u8] = b"\x1b]7501;";
        while !bytes.is_empty() {
            if self.matched == 0 {
                let Some(pos) = bytes.iter().position(|&byte| byte == 0x1b) else {
                    break;
                };
                bytes = &bytes[pos..];
            }
            let byte = bytes[0];
            bytes = &bytes[1..];
            if self.waiting_for_st {
                self.waiting_for_st = false;
                if byte == b'\\' {
                    self.finish(marker, false, &mut report);
                    continue;
                }
                // The escape started a new sequence instead of ending the report.
                self.path.clear();
                self.matched = 1;
            }
            if self.matched == 2 {
                self.kind = byte;
            }
            let prefix = match self.kind {
                b'6' => AGENT,
                b'7' => STATUS,
                _ => NOTIFICATION,
            };
            if self.matched < prefix.len() {
                if byte == prefix[self.matched] {
                    self.matched += 1;
                } else {
                    self.matched = usize::from(byte == 0x1b);
                }
            } else {
                match byte {
                    0x07 => self.finish(marker, true, &mut report),
                    0x1b => self.waiting_for_st = true,
                    0..=31 | 127 => {
                        self.path.clear();
                        self.matched = 0;
                    }
                    _ if self.path.len() < 128 * 1024 => self.path.push(byte),
                    _ => {
                        self.path.clear();
                        self.matched = 0;
                    }
                }
            }
        }
    }

    fn finish(&mut self, marker: &str, bell: bool, report: &mut impl FnMut(Report)) {
        if let Ok(text) = std::str::from_utf8(&self.path) {
            if self.kind == b'7' {
                if text == "?" {
                    report(Report::StatusQuery(bell));
                } else {
                    let field = |name: &str| text.split(':').find_map(|p| p.strip_prefix(name));
                    let app = match field("app=") {
                        Some("codex") => Some(Some(Foreground::Codex)),
                        Some("agy" | "antigravity" | "antigravity-cli") => {
                            Some(Some(Foreground::Agy))
                        }
                        Some("kimi" | "kimi-code") => Some(Some(Foreground::Kimi)),
                        Some("grok" | "grok-build") => Some(Some(Foreground::Grok)),
                        None => Some(None),
                        Some(_) => None,
                    };
                    let value = match field("state=") {
                        Some("blocked") => Some("等待用户"),
                        Some("working") => Some("运行中"),
                        Some("error") => Some("出错"),
                        Some("idle" | "done") => Some("就绪"),
                        Some("clear") => Some(""),
                        _ => None,
                    };
                    if let (Some(app), Some(value)) = (app, value) {
                        if let Some(child_id) = field("id=").filter(|id| !id.is_empty()) {
                            report(Report::ChildStatus(app, child_id.to_owned(), value));
                        } else {
                            report(Report::Status(app, value, StatusSource::Osc7501));
                        }
                    }
                }
            } else if self.kind == b'6' {
                if let Some((session, name)) = text.split_once('=')
                    && session == marker
                {
                    if let Some((agent, rest)) = name.split_once('=') {
                        let (run, explicit) = match rest.split_once('=') {
                            Some((r, id)) => (r, Some(id)),
                            None => (rest, None),
                        };
                        if crate::hook_wire::valid_run(run) {
                            if agent == "agy-prepare" {
                                report(Report::PrepareAgy(run.into()));
                            }
                            let parsed = match agent {
                                "codex" => Some(Foreground::Codex),
                                "agy" => Some(Foreground::Agy),
                                "kimi" => Some(Foreground::Kimi),
                                "grok" => Some(Foreground::Grok),
                                _ => None,
                            };
                            if let Some(parsed) = parsed {
                                let explicit = explicit
                                    .filter(|id| {
                                        crate::hook_wire::valid_session_id(parsed.name(), id)
                                    })
                                    .map(|id| {
                                        if matches!(parsed, Foreground::Codex | Foreground::Agy) {
                                            id.to_ascii_lowercase()
                                        } else {
                                            id.to_owned()
                                        }
                                    });
                                report(Report::Launch(parsed, run.into(), explicit));
                            }
                        }
                    } else {
                        match name {
                            "codex" => report(Report::Foreground(Some(Foreground::Codex))),
                            "agy" => report(Report::Foreground(Some(Foreground::Agy))),
                            "kimi" => report(Report::Foreground(Some(Foreground::Kimi))),
                            "grok" => report(Report::Foreground(Some(Foreground::Grok))),
                            "shell" => report(Report::Foreground(None)),
                            "resume-start" => report(Report::RestoreStarted),
                            "resume-ended" => report(Report::RestoreEnded(RestoreEnd::Finished)),
                            "resume-unavailable" => {
                                report(Report::RestoreEnded(RestoreEnd::Unavailable))
                            }
                            "resume-exhausted" => {
                                report(Report::RestoreEnded(RestoreEnd::Exhausted))
                            }
                            _ => (),
                        }
                    }
                }
            } else if text == "Antigravity CLI is ready for input" {
                report(Report::Status(
                    Some(Foreground::Agy),
                    "就绪",
                    StatusSource::Osc9Ready,
                ));
            } else if let Some(text) = text.strip_prefix("9;") {
                let path = Path::new(
                    text.strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .unwrap_or(text),
                );
                if path.is_absolute() {
                    report(Report::Directory(path.to_owned()));
                }
            }
        }
        self.path.clear();
        self.matched = 0;
    }
}
