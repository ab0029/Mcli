use crate::hook_wire::valid_session_id;
use serde_json::Value;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const MAX_LINE: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Key {
    pub agent: String,
    pub id: String,
}
impl Key {
    pub fn new(agent: &str, id: &str) -> Option<Self> {
        (matches!(agent, "codex" | "agy" | "kimi" | "grok") && valid_session_id(agent, id)).then(
            || Self {
                agent: agent.into(),
                id: if matches!(agent, "codex" | "agy") {
                    id.to_ascii_lowercase()
                } else {
                    id.into()
                },
            },
        )
    }
    pub fn label(&self) -> &'static str {
        match self.agent.as_str() {
            "codex" => "Codex",
            "agy" => "agy",
            "kimi" => "Kimi",
            "grok" => "Grok",
            _ => "Agent",
        }
    }
    pub fn resume(&self) -> String {
        match self.agent.as_str() {
            "codex" => format!("codex resume {}", self.id),
            "agy" => format!("agy --conversation {}", self.id),
            "kimi" => format!("kimi --session {}", self.id),
            "grok" => format!("grok --resume {}", self.id),
            _ => String::new(),
        }
    }
}

#[derive(Clone)]
pub struct Roots {
    pub codex: PathBuf,
    pub agy: PathBuf,
    pub kimi: PathBuf,
    pub grok: PathBuf,
}

impl Roots {
    pub fn installed() -> Option<Self> {
        let home = std::env::home_dir()?;
        Some(Self {
            codex: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex")),
            agy: home.join(".gemini/antigravity-cli/brain"),
            kimi: std::env::var_os("KIMI_CODE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".kimi-code")),
            grok: std::env::var_os("GROK_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".grok")),
        })
    }
    pub fn candidate(&self, key: &Key, source: &Path) -> bool {
        if !source.is_absolute()
            || source
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return false;
        }
        let path = crate::workspace::project_key(source);
        match key.agent.as_str() {
            "agy" => {
                let dir = crate::workspace::project_key(&self.agy)
                    .join(&key.id)
                    .join(".system_generated/logs");
                path.parent() == Some(dir.as_path())
                    && matches!(
                        path.file_name().and_then(|n| n.to_str()),
                        Some("transcript.jsonl" | "transcript_full.jsonl")
                    )
            }
            "kimi" => {
                let sessions = crate::workspace::project_key(&self.kimi).join("sessions");
                path.file_name().and_then(|n| n.to_str()) == Some("wire.jsonl")
                    && path
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        == Some(key.id.as_str())
                    && path.parent().and_then(Path::parent).and_then(Path::parent)
                        == Some(sessions.as_path())
            }
            "grok" => {
                let sessions = crate::workspace::project_key(&self.grok).join("sessions");
                path.file_name().and_then(|n| n.to_str()) == Some("updates.jsonl")
                    && path
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        == Some(key.id.as_str())
                    && path.parent().and_then(Path::parent).and_then(Path::parent)
                        == Some(sessions.as_path())
            }
            _ => {
                let root = crate::workspace::project_key(&self.codex);
                let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                (path.starts_with(root.join("sessions"))
                    || path.starts_with(root.join("archived_sessions")))
                    && filename.starts_with("rollout-")
                    && filename.ends_with(&format!("-{}.jsonl", key.id))
            }
        }
    }
    // Validate before accessing the filesystem, then resolve redirects and check again.
    pub fn path(&self, key: &Key, source: &Path) -> io::Result<PathBuf> {
        if !self.candidate(key, source) {
            return Err(invalid("记录路径与官方对话不匹配"));
        }
        let path = source.canonicalize()?;
        let mut resolved = self.clone();
        match key.agent.as_str() {
            "agy" => resolved.agy = self.agy.canonicalize()?,
            "kimi" => resolved.kimi = self.kimi.canonicalize()?,
            "grok" => resolved.grok = self.grok.canonicalize()?,
            _ => resolved.codex = self.codex.canonicalize()?,
        }
        if resolved.candidate(key, &path) {
            Ok(path)
        } else {
            Err(invalid("记录路径已重定向到其他位置"))
        }
    }
}

/// Match xAI Grok Build's `encode_cwd_dirname` (`xai-grok-config/src/paths.rs`).
pub fn grok_encode_cwd_dirname(cwd: &str) -> String {
    const MAX_DIRNAME_BYTES: usize = 255;
    let encoded = percent_encode(cwd);
    if encoded.len() <= MAX_DIRNAME_BYTES {
        return encoded;
    }
    let hex = blake3::hash(cwd.as_bytes()).to_hex();
    let leaf = Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let slug: String = leaf
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "workspace" } else { slug };
    format!("{slug}-{}", &hex[..16.min(hex.len())])
}

/// Match xAI Grok Build's `decode_cwd_from_dirname` (`xai-grok-config/src/paths.rs`).
pub fn grok_decode_cwd_from_dirname(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_str()?;
    if let Some(decoded) = percent_decode(name)
        && (decoded.starts_with('/') || decoded.as_bytes().get(1) == Some(&b':'))
    {
        return Some(decoded);
    }
    std::fs::read_to_string(dir.join(".cwd"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn percent_encode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[(byte >> 4) as usize]));
            out.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    out
}

fn percent_decode(input: &str) -> Option<String> {
    let mut out = Vec::with_capacity(input.len());
    let mut bytes = input.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = char::from(bytes.next()?).to_digit(16)?;
            let lo = char::from(bytes.next()?).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

pub fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

pub fn snippet(text: &str) -> String {
    let mut chars = text
        .split_whitespace()
        .flat_map(|word| std::iter::once(' ').chain(word.chars()))
        .skip(1);
    let mut result: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

pub struct Prompt {
    pub key: String,
    pub time: Option<String>,
    pub text: String,
}

pub enum Row {
    Header(String),
    Prompt(Prompt),
    Started,
    Reply(String),
    Complete(Option<String>),
    Interrupted,
    Failed,
    Other,
}

pub fn parse(key: &Key, bytes: &[u8]) -> io::Result<Row> {
    let row: Value = serde_json::from_slice(bytes).map_err(|e| invalid(&e.to_string()))?;
    if !row.is_object() {
        return Err(invalid("无法识别此版本的官方记录"));
    }
    if key.agent == "agy" {
        return agy_row(&row);
    }
    if key.agent == "kimi" {
        return kimi_row(&row);
    }
    if key.agent == "grok" {
        return grok_row(&row);
    }
    let kind = row["type"]
        .as_str()
        .ok_or_else(|| invalid("Codex 记录缺少类型"))?;
    let data = &row["payload"];
    if kind == "session_meta" {
        if data["source"].get("subagent").is_some()
            || data["thread_source"].get("subagent").is_some()
        {
            return Err(invalid("子 Agent 记录"));
        }
        return Ok(Row::Header(data["id"].as_str().unwrap_or("").to_owned()));
    }
    if kind != "event_msg" {
        return Ok(Row::Other);
    }
    Ok(match data["type"].as_str() {
        Some("item_completed") if data["item"]["type"] == "UserMessage" => {
            let item = &data["item"];
            let id = item["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| format!("id:{s}"))
                .unwrap_or_default();
            let content = item["content"]
                .as_array()
                .ok_or_else(|| invalid("无法识别 Codex 用户消息"))?;
            let mut parts = Vec::new();
            for part in content {
                match part["type"].as_str() {
                    Some("text") => parts.push(
                        part["text"]
                            .as_str()
                            .ok_or_else(|| invalid("Codex 用户消息缺少正文"))?,
                    ),
                    Some("image" | "local_image" | "localImage") => parts.push("[图片]"),
                    _ => return Err(invalid("此版本的 Codex 消息内容待核对")),
                }
            }
            Row::Prompt(Prompt {
                key: id,
                time: row["timestamp"].as_str().map(str::to_owned),
                text: parts.join("\n"),
            })
        }
        Some("task_started") => Row::Started,
        Some("task_complete") => Row::Complete(
            data["last_agent_message"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        ),
        Some("turn_aborted" | "task_aborted") => Row::Interrupted,
        Some("error") => Row::Failed,
        _ => Row::Other,
    })
}

fn agy_row(row: &Value) -> io::Result<Row> {
    let kind = row["type"]
        .as_str()
        .ok_or_else(|| invalid("agy 记录缺少类型"))?;
    let content = row["content"].as_str().unwrap_or("");
    if kind == "USER_INPUT" && row["source"] == "USER_EXPLICIT" {
        // CLI 1.5 attaches machine-generated metadata after the user envelope.
        let text = if let Some(body) = content
            .strip_prefix("<USER_REQUEST>\r\n")
            .or_else(|| content.strip_prefix("<USER_REQUEST>\n"))
        {
            let mut end = None;
            for close in ["\r\n</USER_REQUEST>", "\n</USER_REQUEST>"] {
                if let Some((prompt, suffix)) = body.rsplit_once(close)
                    && (suffix.trim().is_empty()
                        || suffix.trim_start().starts_with("<ADDITIONAL_METADATA>")
                        || suffix.trim_start().starts_with("<USER_SETTINGS_CHANGE>"))
                {
                    end = Some(prompt);
                    break;
                }
            }
            end.ok_or_else(|| invalid("agy 用户消息封装待核对"))?
        } else {
            content
        };
        return Ok(Row::Prompt(Prompt {
            key: row["step_index"]
                .as_u64()
                .map(|n| format!("step:{n}"))
                .unwrap_or_default(),
            time: row["created_at"].as_str().map(str::to_owned),
            text: text.to_owned(),
        }));
    }
    if kind == "PLANNER_RESPONSE"
        && row["source"] == "MODEL"
        && row["status"] == "DONE"
        && !content.is_empty()
    {
        return Ok(Row::Reply(content.into()));
    }
    Ok(Row::Other)
}

fn epoch_to_iso(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str().filter(|s| !s.is_empty()) {
        return Some(s.to_owned());
    }
    let raw = value
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| if n > 1e12 { n / 1000.0 } else { n })?;
    let secs = raw.floor() as i64;
    let millis = ((raw - raw.floor()) * 1000.0).round() as u32;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant civil_from_days algorithm (UTC).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    Some(format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z"
    ))
}

fn kimi_parts_text(parts: &Value) -> String {
    let Some(arr) = parts.as_array() else {
        return parts.as_str().unwrap_or("").to_owned();
    };
    let mut out = Vec::new();
    for part in arr {
        if let Some(s) = part.as_str() {
            if !s.is_empty() {
                out.push(s);
            }
            continue;
        }
        match part["type"].as_str() {
            Some("text") => {
                if let Some(t) = part["text"].as_str().filter(|s| !s.is_empty()) {
                    out.push(t);
                }
            }
            Some("image" | "image_url" | "local_image" | "video" | "audio") => {
                out.push("[媒体]");
            }
            _ => (),
        }
    }
    out.join("\n")
}

fn kimi_row(row: &Value) -> io::Result<Row> {
    let Some(kind) = row["type"].as_str() else {
        return Ok(Row::Other);
    };
    match kind {
        "turn.prompt" => {
            if row["origin"]
                .as_str()
                .is_some_and(|o| o != "user" && o != "plugin_command")
            {
                return Ok(Row::Other);
            }
            let text = kimi_parts_text(&row["input"]);
            if text.trim().is_empty() {
                return Ok(Row::Other);
            }
            let key = row["promptId"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| format!("prompt:{s}"))
                .or_else(|| row["turnId"].as_u64().map(|t| format!("turn:{t}")))
                .unwrap_or_default();
            Ok(Row::Prompt(Prompt {
                key,
                time: epoch_to_iso(&row["time"]),
                text,
            }))
        }
        "turn_begin" => {
            let text = row["userInput"].as_str().unwrap_or("");
            if text.trim().is_empty() {
                return Ok(Row::Other);
            }
            Ok(Row::Prompt(Prompt {
                key: String::new(),
                time: epoch_to_iso(&row["time"]),
                text: text.to_owned(),
            }))
        }
        "context.append_message" => {
            let msg = &row["message"];
            if msg["role"] == "assistant" {
                let text = kimi_parts_text(&msg["content"]);
                if !text.trim().is_empty() {
                    return Ok(Row::Reply(text));
                }
            }
            Ok(Row::Other)
        }
        "turn.ended" => match row["reason"].as_str() {
            Some("completed") => Ok(Row::Complete(None)),
            Some("cancelled") => Ok(Row::Interrupted),
            Some("failed" | "blocked") => Ok(Row::Failed),
            _ => Ok(Row::Other),
        },
        "turn.cancel" => Ok(Row::Interrupted),
        _ => Ok(Row::Other),
    }
}

fn grok_row(row: &Value) -> io::Result<Row> {
    let method = row["method"].as_str();
    let params = if row.get("params").is_some() {
        &row["params"]
    } else {
        row
    };
    let update = &params["update"];
    if method == Some("_x.ai/session/update") {
        let kind = update["sessionUpdate"]
            .as_str()
            .or_else(|| update["type"].as_str())
            .unwrap_or("");
        return Ok(match kind {
            "turn_ended" | "TurnEnded" => match update["outcome"].as_str() {
                Some("cancelled" | "interrupted") => Row::Interrupted,
                Some("failed" | "error") => Row::Failed,
                _ => Row::Complete(None),
            },
            _ => Row::Other,
        });
    }
    match update["sessionUpdate"].as_str() {
        Some("user_message_chunk") => {
            let content = &update["content"];
            if content["_meta"].get("bash_command").is_some()
                || content["_meta"].get("bashCommand").is_some()
                || update["_meta"]["host_turn"] == true
                || update["_meta"]["hostTurn"] == true
            {
                return Ok(Row::Other);
            }
            if content["type"] != "text" {
                return Ok(Row::Other);
            }
            let text = content["text"].as_str().unwrap_or("");
            if text.trim().is_empty() {
                return Ok(Row::Other);
            }
            let key = update["_meta"]["prompt_index"]
                .as_u64()
                .or_else(|| update["_meta"]["promptIndex"].as_u64())
                .map(|idx| format!("prompt:{idx}"))
                .unwrap_or_default();
            Ok(Row::Prompt(Prompt {
                key,
                time: epoch_to_iso(&row["timestamp"]),
                text: text.to_owned(),
            }))
        }
        Some("agent_message_chunk") => {
            let content = &update["content"];
            if content["type"] == "text"
                && let Some(text) = content["text"].as_str().filter(|s| !s.trim().is_empty())
            {
                return Ok(Row::Reply(text.to_owned()));
            }
            Ok(Row::Other)
        }
        _ => Ok(Row::Other),
    }
}

// A partial UTF-8 sequence or a half-written JSON line is retried from the same byte offset.
pub fn next_row(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<bool> {
    line.clear();
    let n = reader.take((MAX_LINE + 1) as u64).read_until(b'\n', line)?;
    if n > MAX_LINE {
        return Err(invalid("单条官方记录过大，已暂停此对话的同步"));
    }
    Ok(n != 0 && line.last() == Some(&b'\n'))
}

pub fn body(
    roots: &Roots,
    key: &Key,
    source: &Path,
    offset: u64,
    len: usize,
    digest: &[u8],
) -> io::Result<String> {
    let mut reader = BufReader::new(File::open(roots.path(key, source)?)?);
    body_at(&mut reader, key, offset, len, digest)
}

pub fn body_at(
    reader: &mut BufReader<File>,
    key: &Key,
    offset: u64,
    len: usize,
    digest: &[u8],
) -> io::Result<String> {
    if len > MAX_LINE {
        return Err(invalid("消息长度无效"));
    }
    reader.seek(SeekFrom::Start(offset))?;
    let mut line = Vec::new();
    if !next_row(reader, &mut line)? || line.len() != len {
        return Err(invalid("原文位置已变更"));
    }
    match parse(key, &line)? {
        Row::Prompt(prompt) if blake3::hash(prompt.text.as_bytes()).as_bytes() == digest => {
            Ok(prompt.text)
        }
        _ => Err(invalid("原文已变更，请重新同步")),
    }
}

/// Structured prompt/reply turn extracted read-only from an official transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub prompt: String,
    pub reply: Option<String>,
}

/// Read-only extraction of the latest assistant reply text after the last user prompt.
/// Handles all four official formats (Codex `task_complete`, agy `PLANNER_RESPONSE`,
/// Kimi `context.append_message`, and Grok streaming `agent_message_chunk`).
pub fn latest_reply(roots: &Roots, key: &Key, source: &Path) -> io::Result<Option<String>> {
    let mut reader = BufReader::new(File::open(roots.path(key, source)?)?);
    let mut line = Vec::new();
    let mut last_reply: Option<String> = None;
    let mut grok_chunks = String::new();
    let is_grok = key.agent == "grok";

    while next_row(&mut reader, &mut line)? {
        let Ok(parsed) = parse(key, &line) else {
            continue;
        };
        match parsed {
            Row::Prompt(_) => {
                last_reply = None;
                grok_chunks.clear();
            }
            Row::Reply(text) => {
                if is_grok {
                    grok_chunks.push_str(&text);
                } else if !text.trim().is_empty() {
                    last_reply = Some(text);
                }
            }
            Row::Complete(Some(text)) => {
                if !text.trim().is_empty() {
                    last_reply = Some(text);
                }
            }
            _ => (),
        }
    }

    if is_grok {
        let trimmed = grok_chunks.trim();
        Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
    } else {
        Ok(last_reply
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()))
    }
}

/// Read-only extraction of the most recent `limit` prompt/reply turns from an official transcript.
/// Preserves an in-progress trailing turn (`reply: None`) so callers can distinguish
/// "currently generating" from "no completed replies" and still inspect the previous reply.
pub fn recent_turns(
    roots: &Roots,
    key: &Key,
    source: &Path,
    limit: usize,
) -> io::Result<Vec<Turn>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut reader = BufReader::new(File::open(roots.path(key, source)?)?);
    let mut line = Vec::new();
    let mut turns: Vec<Turn> = Vec::new();
    let mut grok_chunks = String::new();
    let is_grok = key.agent == "grok";

    let set_reply = |turns: &mut Vec<Turn>, text: String| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let owned = trimmed.to_owned();
        if let Some(last) = turns.last_mut() {
            last.reply = Some(owned);
        } else {
            turns.push(Turn {
                prompt: String::new(),
                reply: Some(owned),
            });
        }
    };

    while next_row(&mut reader, &mut line)? {
        let Ok(parsed) = parse(key, &line) else {
            continue;
        };
        match parsed {
            Row::Prompt(prompt) => {
                if is_grok && !grok_chunks.is_empty() {
                    let flushed = std::mem::take(&mut grok_chunks);
                    set_reply(&mut turns, flushed);
                }
                turns.push(Turn {
                    prompt: prompt.text.trim().to_owned(),
                    reply: None,
                });
            }
            Row::Reply(text) => {
                if is_grok {
                    grok_chunks.push_str(&text);
                } else {
                    set_reply(&mut turns, text);
                }
            }
            Row::Complete(Some(text)) => {
                set_reply(&mut turns, text);
            }
            _ => (),
        }
    }

    if is_grok && !grok_chunks.is_empty() {
        let flushed = std::mem::take(&mut grok_chunks);
        set_reply(&mut turns, flushed);
    }

    if turns.len() > limit {
        turns.drain(..turns.len() - limit);
    }
    Ok(turns)
}
