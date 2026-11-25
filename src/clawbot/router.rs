use std::collections::VecDeque;

pub const MAX_MESSAGE_CHARS: usize = 3500;
pub const AUTO_SEND_MAX_CHUNKS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteKey {
    Enter,
    Esc,
    Y,
    N,
    Up,
    Down,
    Tab,
    CtrlC,
}

impl RemoteKey {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "enter" | "cr" | "return" => Some(Self::Enter),
            "esc" | "escape" => Some(Self::Esc),
            "y" | "yes" => Some(Self::Y),
            "n" | "no" => Some(Self::N),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "tab" => Some(Self::Tab),
            "ctrl+c" | "ctrlc" | "sigint" => Some(Self::CtrlC),
            _ => None,
        }
    }

    pub fn bytes(self) -> &'static [u8] {
        match self {
            Self::Enter => b"\r",
            Self::Esc => b"\x1b",
            Self::Y => b"y\r",
            Self::N => b"n\r",
            Self::Up => b"\x1b[A",
            Self::Down => b"\x1b[B",
            Self::Tab => b"\t",
            Self::CtrlC => b"\x03",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Enter => "Enter",
            Self::Esc => "Esc",
            Self::Y => "y + Enter",
            Self::N => "n + Enter",
            Self::Up => "Up",
            Self::Down => "Down",
            Self::Tab => "Tab",
            Self::CtrlC => "Ctrl+C",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedCommand {
    Help,
    Ping,
    List,
    Use(Option<String>),
    Status,
    Last {
        count: usize,
    },
    Proxy {
        target: Option<String>,
        restart: bool,
    },
    New {
        target: Option<String>,
        agent: Option<String>,
        proxy: Option<String>,
    },
    Reopen,
    Screen {
        lines: usize,
    },
    Stop,
    More,
    Approve,
    Deny,
    Key(RemoteKey),
    Shell(String),
    UnknownSlash(String),
    Prompt(String),
}

pub fn parse_command(raw: &str) -> ParsedCommand {
    let trimmed = raw.trim();
    if !trimmed.starts_with('/') {
        return ParsedCommand::Prompt(trimmed.to_owned());
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let rest = parts.next().map(str::trim).unwrap_or("");
    let cmd = head.to_ascii_lowercase();
    match cmd.as_str() {
        "/help" | "/h" | "/?" => ParsedCommand::Help,
        "/ping" | "/alive" | "/keepalive" => ParsedCommand::Ping,
        "/list" | "/ls" => ParsedCommand::List,
        "/use" | "/switch" => ParsedCommand::Use((!rest.is_empty()).then(|| rest.to_owned())),
        "/status" | "/st" => ParsedCommand::Status,
        "/last" | "/r" | "/reply" => {
            if rest.is_empty() {
                ParsedCommand::Last { count: 1 }
            } else if let Ok(n) = rest.parse::<usize>() {
                ParsedCommand::Last {
                    count: n.clamp(1, 5),
                }
            } else {
                ParsedCommand::UnknownSlash(format!(
                    "{head} 参数无效，用法：/last [1-5]（或 /r [1-5]）"
                ))
            }
        }
        "/proxy" | "/pr" => parse_proxy_args(rest),
        "/new" => parse_new_args(rest),
        "/reopen" | "/restart" => ParsedCommand::Reopen,
        "/screen" | "/sc" => {
            let lines = rest
                .parse::<usize>()
                .ok()
                .map(|n| n.clamp(5, 80))
                .unwrap_or(30);
            ParsedCommand::Screen { lines }
        }
        "/stop" | "/cancel" => ParsedCommand::Stop,
        "/more" => ParsedCommand::More,
        "/approve" | "/yes" | "/y" => ParsedCommand::Approve,
        "/deny" | "/reject" | "/no" | "/n" => ParsedCommand::Deny,
        "/key" => match RemoteKey::parse(rest) {
            Some(k) => ParsedCommand::Key(k),
            None => ParsedCommand::UnknownSlash(format!(
                "/key 参数无效，支持：enter、esc、y、n、up、down、tab、ctrl+c"
            )),
        },
        "/sh" | "/shell" => ParsedCommand::Shell(rest.to_owned()),
        _ => ParsedCommand::UnknownSlash(head.to_owned()),
    }
}

fn parse_proxy_args(rest: &str) -> ParsedCommand {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return ParsedCommand::Proxy {
            target: None,
            restart: false,
        };
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() >= 2
        && matches!(
            tokens.last().unwrap().to_ascii_lowercase().as_str(),
            "restart" | "force" | "-f"
        )
    {
        let target = tokens[..tokens.len() - 1].join(" ");
        ParsedCommand::Proxy {
            target: (!target.is_empty()).then_some(target),
            restart: true,
        }
    } else {
        ParsedCommand::Proxy {
            target: Some(trimmed.to_owned()),
            restart: false,
        }
    }
}

fn parse_new_args(rest: &str) -> ParsedCommand {
    let mut work = rest.trim();
    let mut proxy = None;
    if let Some((lhs, rhs)) = work.rsplit_once(char::is_whitespace) {
        if let Some(val) = strip_proxy_option(rhs) {
            proxy = Some(val);
            work = lhs.trim();
        }
    } else if let Some(val) = strip_proxy_option(work) {
        return ParsedCommand::New {
            target: None,
            agent: None,
            proxy: Some(val),
        };
    }
    if work.is_empty() {
        return ParsedCommand::New {
            target: None,
            agent: None,
            proxy,
        };
    }
    let tokens: Vec<&str> = work.split_whitespace().collect();
    if tokens.len() == 1 {
        let low = tokens[0].to_ascii_lowercase();
        if matches!(low.as_str(), "codex" | "agy" | "kimi" | "grok" | "shell") {
            return ParsedCommand::New {
                target: None,
                agent: Some(low),
                proxy,
            };
        }
        return ParsedCommand::New {
            target: Some(work.trim_matches('"').to_owned()),
            agent: None,
            proxy,
        };
    }
    let last_low = tokens.last().unwrap().to_ascii_lowercase();
    if matches!(
        last_low.as_str(),
        "codex" | "agy" | "kimi" | "grok" | "shell"
    ) {
        // Preserve spaces in path before the trailing agent name.
        let target = work
            .rsplit_once(char::is_whitespace)
            .map(|(lhs, _)| lhs.trim().trim_matches('"').to_owned())
            .filter(|s| !s.is_empty());
        ParsedCommand::New {
            target,
            agent: Some(last_low),
            proxy,
        }
    } else {
        ParsedCommand::New {
            target: Some(work.trim_matches('"').to_owned()),
            agent: None,
            proxy,
        }
    }
}

fn strip_proxy_option(token: &str) -> Option<String> {
    let (k, v) = token.split_once('=')?;
    if k.eq_ignore_ascii_case("proxy") || k.eq_ignore_ascii_case("p") {
        let val = v.trim();
        (!val.is_empty()).then(|| val.to_owned())
    } else {
        None
    }
}

pub fn split_chunks(text: &str, max_chars: usize) -> Vec<String> {
    let clean = text.trim();
    if clean.is_empty() {
        return Vec::new();
    }
    let limit = max_chars.max(64);
    let mut chunks = Vec::new();
    let mut remaining = clean;

    while !remaining.is_empty() {
        let total_chars = remaining.chars().count();
        if total_chars <= limit {
            chunks.push(remaining.to_owned());
            break;
        }
        // Find byte offset of `limit` chars.
        let hard_byte = remaining
            .char_indices()
            .nth(limit)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());
        let window = &remaining[..hard_byte];
        let min_preferred = hard_byte / 2;
        let split_at = window
            .rfind("\n\n")
            .filter(|&i| i >= min_preferred)
            .or_else(|| window.rfind('\n').filter(|&i| i >= min_preferred))
            .unwrap_or(hard_byte);
        let piece = remaining[..split_at].trim_end();
        if !piece.is_empty() {
            chunks.push(piece.to_owned());
        }
        remaining = remaining[split_at..].trim_start();
    }
    chunks
}

#[derive(Default)]
pub struct MoreQueue {
    remaining: VecDeque<String>,
}

impl MoreQueue {
    /// Splits `text` into chunks, returns up to `AUTO_SEND_MAX_CHUNKS` immediate messages
    /// (with a `/more` hint on the last immediate message if truncated), and queues the rest.
    pub fn prepare(&mut self, text: &str) -> Vec<String> {
        self.remaining.clear();
        let mut all = split_chunks(text, MAX_MESSAGE_CHARS - 80);
        if all.len() <= AUTO_SEND_MAX_CHUNKS {
            return all;
        }
        let rest: Vec<String> = all.drain(AUTO_SEND_MAX_CHUNKS..).collect();
        let rest_count = rest.len();
        self.remaining = rest.into();
        if let Some(last) = all.last_mut() {
            last.push_str(&format!(
                "\n\n-- 内容较长，剩余 {rest_count} 段，发送 /more 查看下一段 --"
            ));
        }
        all
    }

    pub fn next_chunk(&mut self) -> Option<String> {
        let mut chunk = self.remaining.pop_front()?;
        let left = self.remaining.len();
        if left > 0 {
            chunk.push_str(&format!(
                "\n\n（剩余 {left} 段，继续发送 /more 查看下一段）"
            ));
        }
        Some(chunk)
    }

    #[cfg(test)]
    pub fn remaining_len(&self) -> usize {
        self.remaining.len()
    }
}

pub fn help_text(bound_desc: &str, allow_raw_shell: bool) -> String {
    let sh_note = if allow_raw_shell {
        "/sh <命令> -- 在普通 PowerShell 执行单条命令（已开启）"
    } else {
        "/sh <命令> -- 执行 PowerShell 命令（当前已关闭，可在桌面端设置开启）"
    };
    format!(
        "Mcli 微信远程控制指令：\n\
/ping -- 检查在线状态并刷新 24 小时会话令牌（不打扰 Agent）\n\
/list -- 查看全部项目、终端编号与运行状态\n\
/use <编号|关键词> -- 切换当前远程控制目标终端\n\
/status -- 查看当前目标终端状态与摘要\n\
/last [1-5]（或 /r）-- 读取当前 Agent 最新一条干净完整回复（或最近 2-5 轮对话）\n\
/proxy [预设名|编号|off|sys] [restart] -- 查看或一键切换代理（Agent 按原 ID 平滑续接）\n\
/new [路径|项目名] [codex|agy|kimi|grok|shell] [proxy=<名>] -- 新建终端或启动 Agent\n\
/reopen -- 在原目录重开已退出/已关闭的绑定终端（若有官方对话则按原 ID 续接）\n\
/screen [行数] -- 查看当前终端底部纯文本屏显（默认 30 行，适合看报错/菜单）\n\
/stop -- 向当前终端发送 Ctrl+C 中断任务\n\
/approve 或 /deny -- 在「等待用户」时显式确认或拒绝\n\
/key <enter|esc|y|n|up|down|tab|ctrl+c> -- 发送按键\n\
/more -- 查看超长回复的剩余分段\n\
{sh_note}\n\
直接发送普通文本 -- 投递给当前绑定（或默认）Agent 执行并等待结果回传。\n\n\
当前目标：{bound_desc}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_builtin_commands_and_plain_prompts() {
        assert_eq!(parse_command("/help"), ParsedCommand::Help);
        assert_eq!(parse_command("/LIST"), ParsedCommand::List);
        assert_eq!(
            parse_command("/use 2"),
            ParsedCommand::Use(Some("2".into()))
        );
        assert_eq!(parse_command("/use"), ParsedCommand::Use(None));
        assert_eq!(parse_command("/status"), ParsedCommand::Status);
        assert_eq!(parse_command("/last"), ParsedCommand::Last { count: 1 });
        assert_eq!(parse_command("/r"), ParsedCommand::Last { count: 1 });
        assert_eq!(parse_command("/reply 3"), ParsedCommand::Last { count: 3 });
        assert_eq!(parse_command("/last 0"), ParsedCommand::Last { count: 1 });
        assert_eq!(parse_command("/last 10"), ParsedCommand::Last { count: 5 });
        assert!(matches!(
            parse_command("/r abc"),
            ParsedCommand::UnknownSlash(_)
        ));
        assert_eq!(
            parse_command("/new L:\\AI\\My App codex"),
            ParsedCommand::New {
                target: Some(r"L:\AI\My App".into()),
                agent: Some("codex".into()),
                proxy: None,
            }
        );
        assert_eq!(
            parse_command("/new L:\\AI\\My App codex proxy=p1"),
            ParsedCommand::New {
                target: Some(r"L:\AI\My App".into()),
                agent: Some("codex".into()),
                proxy: Some("p1".into()),
            }
        );
        assert_eq!(
            parse_command("/new agy"),
            ParsedCommand::New {
                target: None,
                agent: Some("agy".into()),
                proxy: None,
            }
        );
        assert_eq!(
            parse_command("/proxy"),
            ParsedCommand::Proxy {
                target: None,
                restart: false,
            }
        );
        assert_eq!(
            parse_command("/proxy p1"),
            ParsedCommand::Proxy {
                target: Some("p1".into()),
                restart: false,
            }
        );
        assert_eq!(
            parse_command("/proxy p2 restart"),
            ParsedCommand::Proxy {
                target: Some("p2".into()),
                restart: true,
            }
        );
        assert_eq!(
            parse_command("/screen"),
            ParsedCommand::Screen { lines: 30 }
        );
        assert_eq!(
            parse_command("/screen 200"),
            ParsedCommand::Screen { lines: 80 }
        );
        assert_eq!(parse_command("/stop"), ParsedCommand::Stop);
        assert_eq!(parse_command("/more"), ParsedCommand::More);
        assert_eq!(parse_command("/approve"), ParsedCommand::Approve);
        assert_eq!(parse_command("/deny"), ParsedCommand::Deny);
        assert_eq!(
            parse_command("/key esc"),
            ParsedCommand::Key(RemoteKey::Esc)
        );
        assert_eq!(
            parse_command("/sh git status"),
            ParsedCommand::Shell("git status".into())
        );
        assert_eq!(
            parse_command("帮我检查 Cargo.toml"),
            ParsedCommand::Prompt("帮我检查 Cargo.toml".into())
        );
    }

    #[test]
    fn chunks_long_utf8_text_and_queues_more() {
        let mut q = MoreQueue::default();
        let para = format!("{}\n\n", "这是一段中文测试回复内容。".repeat(80));
        let long_text = para.repeat(9);
        let immediate = q.prepare(&long_text);
        assert_eq!(immediate.len(), AUTO_SEND_MAX_CHUNKS);
        assert!(immediate.last().unwrap().contains("/more"));
        assert!(q.remaining_len() >= 1);
        while let Some(next) = q.next_chunk() {
            assert!(next.chars().count() <= MAX_MESSAGE_CHARS);
        }
        assert_eq!(q.remaining_len(), 0);
    }
}
