pub mod bridge;
pub mod protocol;
pub mod qr;
pub mod router;
pub mod service;
pub mod store;

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;

pub const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
pub const MAX_AUDIT_ENTRIES: usize = 20;

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_owned()
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastBoundTarget {
    pub directory: PathBuf,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
}

/// Non-sensitive ClawBot settings stored in `preferences.json`.
/// Never contains `bot_token`, `ilink_user_id`, `ilink_bot_id`, or sync cursors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClawbotPrefs {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub default_project: Option<PathBuf>,
    #[serde(default)]
    pub default_agent: Option<String>,
    #[serde(default)]
    pub allow_raw_shell: bool,
    #[serde(default)]
    pub notify_desktop_tasks: bool,
    #[serde(default = "default_true")]
    pub keepalive_remind: bool,
    #[serde(default)]
    pub last_bound: Option<LastBoundTarget>,
}

impl Default for ClawbotPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            default_project: None,
            default_agent: None,
            allow_raw_shell: false,
            notify_desktop_tasks: false,
            keepalive_remind: true,
            last_bound: None,
        }
    }
}

impl ClawbotPrefs {
    pub fn normalized_base_url(&self) -> &str {
        let trimmed = self.base_url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            DEFAULT_BASE_URL
        } else {
            trimmed
        }
    }

    pub fn normalized_agent(&self) -> Option<&'static str> {
        match self.default_agent.as_deref().map(str::trim) {
            Some("codex") => Some("codex"),
            Some("agy") => Some("agy"),
            Some("kimi") => Some("kimi"),
            Some("grok") => Some("grok"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disabled,
    Unbound,
    WaitingQr {
        qr_url: String,
        scanned: bool,
    },
    Connected {
        user_hint: String,
        paused: bool,
    },
    TakenByOtherWindow,
    Reconnecting {
        user_hint: String,
        attempt: u32,
        reason: String,
    },
    SessionExpired {
        reason: String,
    },
    Error(String),
}

impl ConnectionState {
    pub fn badge_text(&self) -> &'static str {
        match self {
            Self::Disabled => "已关闭",
            Self::Unbound => "未绑定",
            Self::WaitingQr { scanned: false, .. } => "等待扫码",
            Self::WaitingQr { scanned: true, .. } => "已扫码待确认",
            Self::Connected { paused: false, .. } => "已连接",
            Self::Connected { paused: true, .. } => "已暂停",
            Self::TakenByOtherWindow => "其他窗口接管",
            Self::Reconnecting { .. } => "重连中",
            Self::SessionExpired { .. } => "会话失效",
            Self::Error(_) => "异常",
        }
    }

    pub fn is_online(&self) -> bool {
        matches!(self, Self::Connected { paused: false, .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    pub time: String,
    pub kind: String,
    pub target: String,
    pub outcome: String,
}

#[derive(Default)]
pub struct AuditLog {
    entries: VecDeque<AuditEntry>,
}

impl AuditLog {
    pub fn push(
        &mut self,
        kind: impl Into<String>,
        target: impl Into<String>,
        outcome: impl Into<String>,
    ) {
        let entry = AuditEntry {
            time: now_hms(),
            kind: kind.into(),
            target: target.into(),
            outcome: outcome.into(),
        };
        self.entries.push_front(entry);
        while self.entries.len() > MAX_AUDIT_ENTRIES {
            self.entries.pop_back();
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &AuditEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub fn mask_user_id(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return "未知账号".into();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return format!("{}***", chars.iter().take(2).collect::<String>());
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Display Beijing time (UTC+8) in audit entries.
    let local = secs + 8 * 3600;
    let h = (local / 3600) % 24;
    let m = (local / 60) % 60;
    let s = local % 60;
    format!("{h:02}:{m:02}:{s:02}")
}
