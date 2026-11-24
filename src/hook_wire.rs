use serde_json::Value;
use std::path::PathBuf;

pub const MAX_PACKET: usize = 32 * 1024;

pub fn host(marker: &str) -> Option<(u32, String)> {
    let mut fields = marker.split('-');
    if fields.next()? != "mcli" {
        return None;
    }
    let pid: u32 = fields.next()?.parse().ok()?;
    let stamp: u128 = fields.next()?.parse().ok()?;
    let _: u64 = fields.next()?.parse().ok()?;
    (fields.next().is_none() && pid != 0).then(|| (pid, format!("mcli-{pid}-{stamp}")))
}

pub fn pipe_name(host: &str) -> String {
    format!(r"\\.\pipe\{host}-conversations")
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

pub fn valid_session_id(agent: &str, id: &str) -> bool {
    match agent {
        "codex" | "agy" => valid_id(id),
        "kimi" | "grok" => {
            !id.is_empty()
                && id.len() <= 128
                && id != "."
                && id != ".."
                && !id.starts_with('-')
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        }
        _ => false,
    }
}

pub fn valid_run(run: &str) -> bool {
    run.len() == 32 && run.bytes().all(|c| c.is_ascii_hexdigit())
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub marker: String,
    pub run: String,
    pub provider: String,
    pub event: String,
    pub id: String,
    pub path: Option<PathBuf>,
    pub turn: Option<String>,
    pub detail: String,
    pub sent: u64,
    pub question: Option<String>,
}

pub fn timestamp_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

impl Notice {
    #[allow(dead_code)] // The helper reads vendor JSON. The desktop only reads the wire format.
    pub fn from_hook(
        provider: &str,
        event: &str,
        marker: String,
        run: String,
        hook: &Value,
    ) -> Option<Self> {
        host(&marker)?;
        if !valid_run(&run) {
            return None;
        }
        let (id, path, turn, detail) = match provider {
            "codex" => {
                if hook.get("hook_event_name")?.as_str()? != event
                    || !matches!(
                        event,
                        "SessionStart"
                            | "UserPromptSubmit"
                            | "Stop"
                            | "Interrupt"
                            | "PermissionRequest"
                            | "Notification"
                            | "SessionEnd"
                    )
                    || ["agent_id", "agent_type"]
                        .iter()
                        .any(|k| hook[*k].as_str().is_some_and(|s| !s.is_empty()))
                {
                    return None;
                }
                (
                    &hook["session_id"],
                    &hook["transcript_path"],
                    &hook["turn_id"],
                    hook["source"]
                        .as_str()
                        .or_else(|| hook["notification_type"].as_str()),
                )
            }
            "agy" if matches!(event, "SessionStart" | "PreInvocation" | "Stop") => (
                &hook["conversationId"],
                &hook["transcriptPath"],
                &Value::Null,
                if hook["fullyIdle"].as_bool() == Some(false) {
                    Some("background_active")
                } else {
                    hook["terminationReason"].as_str()
                },
            ),
            "agy" if event == "StatusLine" => (
                hook.get("conversation_id")
                    .or_else(|| hook.get("session_id"))?,
                &Value::Null,
                &Value::Null,
                Some(
                    if hook["tool_confirmation_pending"].as_bool() == Some(true) {
                        "waiting"
                    } else {
                        match hook["agent_state"].as_str() {
                            Some("thinking" | "working" | "tool_use") => "working",
                            Some("idle") => "ready",
                            Some("error") => "error",
                            _ => "online",
                        }
                    },
                ),
            ),
            _ => return None,
        };
        let id = id.as_str()?;
        if !valid_id(id) && !(event == "StatusLine" && id.is_empty()) {
            return None;
        }
        let sent = timestamp_micros();
        let question = hook["lastUserInput"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| blake3::hash(s.as_bytes()).to_hex().to_string());
        Some(Self {
            marker,
            run,
            sent,
            question,
            provider: provider.into(),
            event: event.into(),
            id: id.to_ascii_lowercase(),
            path: path.as_str().filter(|s| !s.is_empty()).map(PathBuf::from),
            turn: turn.as_str().filter(|s| s.len() <= 128).map(String::from),
            detail: detail.unwrap_or("").chars().take(64).collect(),
        })
    }

    #[allow(dead_code)] // The desktop receives packets, the hook binary sends them.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!([
            self.marker,
            self.run,
            self.provider,
            self.event,
            self.id,
            self.path,
            self.turn,
            self.detail,
            self.sent,
            self.question
        ]))
        .unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_PACKET {
            return None;
        }
        let (marker, run, provider, event, id, path, turn, detail, sent, question): (
            String,
            String,
            String,
            String,
            String,
            Option<PathBuf>,
            Option<String>,
            String,
            u64,
            Option<String>,
        ) = serde_json::from_slice(bytes).ok()?;
        host(&marker)?;
        let supported = match provider.as_str() {
            "codex" => matches!(
                event.as_str(),
                "SessionStart"
                    | "UserPromptSubmit"
                    | "PermissionRequest"
                    | "Stop"
                    | "Interrupt"
                    | "SessionEnd"
            ),
            "agy" => matches!(
                event.as_str(),
                "SessionStart" | "PreInvocation" | "Stop" | "StatusLine"
            ),
            _ => false,
        };
        if !(supported
            && valid_run(&run)
            && (valid_id(&id) || event == "StatusLine" && id.is_empty()))
            || detail.len() > 256
            || turn.as_ref().is_some_and(|t| t.len() > 128)
            || question
                .as_ref()
                .is_some_and(|q| q.len() != 64 || !q.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return None;
        }
        Some(Self {
            marker,
            run,
            provider,
            event,
            id,
            path,
            turn,
            detail,
            sent,
            question,
        })
    }
}
