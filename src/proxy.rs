use crate::{terminal::SessionId, transcript::Key};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const DIRECT_PROFILE: &str = "__direct__";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyProfile {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub no_proxy: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySettings {
    #[serde(default)]
    pub profiles: Vec<ProxyProfile>,
    /// Global default for newly opened shells:
    /// `None` = follow system environment, `Some("__direct__")` = force direct, `Some(name)` = profile.
    #[serde(default)]
    pub default_profile: Option<String>,
    /// Optional per-Agent auto proxy (`"codex" | "agy" | "kimi" | "grok"` -> profile name or `"__direct__"`).
    #[serde(default)]
    pub agent_defaults: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyTarget {
    System,
    Direct,
    Profile(ProxyProfile),
}

impl ProxyTarget {
    pub fn display_label(&self) -> String {
        match self {
            Self::System => "系统默认".into(),
            Self::Direct => "直连（清除代理）".into(),
            Self::Profile(p) => format!("{} ({})", p.name, sanitize_proxy_summary(&p.url)),
        }
    }

    pub fn short_label(&self) -> String {
        match self {
            Self::System => "系统默认".into(),
            Self::Direct => "直连".into(),
            Self::Profile(p) => {
                let compact = short_host_port(&p.url);
                if compact.is_empty() {
                    p.name.clone()
                } else {
                    format!("{} ({compact})", p.name)
                }
            }
        }
    }

    pub fn shell_arg(&self) -> String {
        match self {
            Self::System => "sys".into(),
            Self::Direct => "off".into(),
            Self::Profile(p) => p.name.clone(),
        }
    }

    pub fn initial_summary(&self) -> String {
        match self {
            Self::System => String::new(),
            Self::Direct => "off".into(),
            Self::Profile(p) => sanitize_proxy_summary(&p.url),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummaryMatch {
    System(Option<String>),
    Direct,
    Profile { name: String, url: String },
    Custom(String),
}

impl SummaryMatch {
    pub fn pill_label(&self) -> String {
        match self {
            Self::System(None) => "系统默认".into(),
            Self::System(Some(url)) => {
                let compact = short_host_port(url);
                if compact.is_empty() {
                    "系统默认".into()
                } else {
                    format!("系统 ({compact})")
                }
            }
            Self::Direct => "直连".into(),
            Self::Profile { name, url } => {
                let compact = short_host_port(url);
                if compact.is_empty() {
                    name.clone()
                } else {
                    format!("{name} ({compact})")
                }
            }
            Self::Custom(url) => {
                let compact = short_host_port(url);
                if compact.is_empty() {
                    "自定义".into()
                } else {
                    format!("自定义 ({compact})")
                }
            }
        }
    }

    pub fn detail_label(&self) -> String {
        match self {
            Self::System(None) => "系统默认（未设置代理）".into(),
            Self::System(Some(url)) => format!("系统默认（{url}）"),
            Self::Direct => "直连（已清除代理变量）".into(),
            Self::Profile { name, url } => format!("{name}（{url}）"),
            Self::Custom(url) => format!("自定义（{url}）"),
        }
    }
}

pub fn normalize_profile_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("预设名称不能为空".into());
    }
    if name.chars().count() > 24 {
        return Err("预设名称最多 24 个字符".into());
    }
    if name.chars().any(|c| {
        c.is_whitespace() || c.is_control() || matches!(c, '\'' | '"' | ';' | '|' | '&' | '=' | '`')
    }) {
        return Err("预设名称不能包含空格、引号或特殊符号（如 ; | & =）".into());
    }
    let low = name.to_ascii_lowercase();
    if matches!(
        low.as_str(),
        "off" | "direct" | "none" | "0" | "sys" | "system" | "reset" | "__direct__" | "list"
    ) {
        return Err(format!("「{name}」是保留名称，请换一个预设名"));
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return Err("预设名称不能为纯数字（数字保留用于快捷编号）".into());
    }
    Ok(name.to_owned())
}

pub fn normalize_proxy_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("代理地址不能为空（可填端口号如 10808 或完整地址）".into());
    }
    if trimmed
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '\'' | '"'))
    {
        return Err("代理地址不能包含空格、引号或控制字符".into());
    }
    if trimmed.bytes().all(|b| b.is_ascii_digit()) {
        let port: u16 = trimmed
            .parse()
            .map_err(|_| "端口号无效，请输入 1-65535 之间的数字".to_owned())?;
        if port == 0 {
            return Err("端口号不能为 0".into());
        }
        return Ok(format!("http://127.0.0.1:{port}"));
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else if trimmed.contains(':') {
        format!("http://{trimmed}")
    } else {
        return Err(
            "代理地址格式无效，请输入端口号（如 10808）或完整地址（如 http://127.0.0.1:10808）"
                .into(),
        );
    };
    let lower = candidate.to_ascii_lowercase();
    let valid_scheme = ["http://", "https://", "socks5://", "socks5h://"]
        .iter()
        .any(|s| lower.starts_with(s));
    if !valid_scheme {
        return Err("仅支持 http://、https://、socks5://、socks5h:// 协议".into());
    }
    let rest = candidate.split_once("://").map(|(_, r)| r).unwrap_or("");
    if rest.is_empty() || rest.starts_with('/') {
        return Err("代理地址缺少主机和端口".into());
    }
    Ok(candidate)
}

pub fn normalize_no_proxy(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed
        .chars()
        .any(|c| c.is_control() || matches!(c, '\'' | '"'))
    {
        return Err("NO_PROXY 不能包含引号或控制字符".into());
    }
    Ok(trimmed.to_owned())
}

/// Strips embedded `user:pass@` credentials and control characters from a proxy URL for safe display/OSC.
pub fn sanitize_proxy_summary(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let cleaned: String = trimmed.chars().filter(|c| !c.is_control()).collect();
    let Some((scheme, rest)) = cleaned.split_once("://") else {
        return cleaned;
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let tail = &rest[authority_end..];
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, hp)| hp)
        .unwrap_or(authority);
    format!("{scheme}://{host_port}{tail}")
}

pub fn short_host_port(url: &str) -> String {
    let sanitized = sanitize_proxy_summary(url);
    let rest = sanitized
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(&sanitized);
    let authority = rest.split('/').next().unwrap_or(rest);
    if let Some(port) = authority
        .strip_prefix("127.0.0.1:")
        .or_else(|| authority.strip_prefix("localhost:"))
        .or_else(|| authority.strip_prefix("[::1]:"))
    {
        port.to_owned()
    } else {
        authority.to_owned()
    }
}

impl ProxySettings {
    pub fn find_profile(&self, query: &str) -> Option<&ProxyProfile> {
        let q = query.trim();
        if q.is_empty() {
            return None;
        }
        if let Ok(index) = q.parse::<usize>()
            && index >= 1
        {
            return self.profiles.get(index - 1);
        }
        self.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(q))
    }

    pub fn resolve_target(&self, query: &str) -> Option<ProxyTarget> {
        let q = query.trim();
        if q.is_empty() {
            return None;
        }
        match q.to_ascii_lowercase().as_str() {
            "sys" | "system" | "reset" => Some(ProxyTarget::System),
            "off" | "direct" | "none" | "0" | "__direct__" => Some(ProxyTarget::Direct),
            _ => self.find_profile(q).cloned().map(ProxyTarget::Profile),
        }
    }

    pub fn default_target(&self) -> Option<ProxyTarget> {
        let name = self.default_profile.as_deref()?;
        if name == DIRECT_PROFILE {
            return Some(ProxyTarget::Direct);
        }
        self.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .cloned()
            .map(ProxyTarget::Profile)
    }

    pub fn agent_default(&self, agent: &str) -> Option<&str> {
        self.agent_defaults
            .get(&agent.to_ascii_lowercase())
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub fn set_agent_default(&mut self, agent: &str, value: Option<String>) {
        let key = agent.to_ascii_lowercase();
        match value.filter(|v| !v.trim().is_empty()) {
            Some(v) => {
                self.agent_defaults.insert(key, v);
            }
            None => {
                self.agent_defaults.remove(&key);
            }
        }
    }

    pub fn upsert_profile(
        &mut self,
        name: &str,
        url: &str,
        no_proxy: &str,
    ) -> Result<ProxyProfile, String> {
        let name = normalize_profile_name(name)?;
        let url = normalize_proxy_url(url)?;
        let no_proxy = normalize_no_proxy(no_proxy)?;
        let profile = ProxyProfile {
            name: name.clone(),
            url,
            no_proxy,
        };
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(&name))
        {
            *existing = profile.clone();
        } else {
            self.profiles.push(profile.clone());
        }
        Ok(profile)
    }

    pub fn remove_profile(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| !p.name.eq_ignore_ascii_case(name));
        let removed = self.profiles.len() != before;
        if removed {
            if self
                .default_profile
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case(name))
            {
                self.default_profile = None;
            }
            self.agent_defaults
                .retain(|_, v| !v.eq_ignore_ascii_case(name));
        }
        removed
    }

    pub fn match_summary(&self, summary: &str) -> SummaryMatch {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return SummaryMatch::System(None);
        }
        if trimmed.eq_ignore_ascii_case("off") {
            return SummaryMatch::Direct;
        }
        if let Some(sys_url) = trimmed.strip_prefix("sys:") {
            let clean = sanitize_proxy_summary(sys_url);
            return SummaryMatch::System((!clean.is_empty()).then_some(clean));
        }
        let clean = sanitize_proxy_summary(trimmed);
        if let Some(p) = self
            .profiles
            .iter()
            .find(|p| sanitize_proxy_summary(&p.url).eq_ignore_ascii_case(&clean))
        {
            SummaryMatch::Profile {
                name: p.name.clone(),
                url: clean,
            }
        } else {
            SummaryMatch::Custom(clean)
        }
    }
}

pub fn catalog_path(data_dir: &Path) -> PathBuf {
    data_dir.join("proxy-profiles.json")
}

pub fn write_catalog(path: &Path, settings: &ProxySettings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(settings)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path).or_else(|_| {
        let _ = fs::remove_file(path);
        fs::rename(&tmp, path)
    })
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(windows)]
fn dpapi_protect_no_entropy(plaintext: &[u8]) -> io::Result<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(plaintext.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "代理配置过长"))?,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            null(),
            null(),
            null_mut(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let cipher =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(cipher)
}

/// Produces a DPAPI-sealed Base64 string compatible with `shell_proxy.ps1`
/// (`ProtectedData::Unprotect($blob, $null, 'CurrentUser')`).
pub fn seal_target(target: &ProxyTarget) -> io::Result<String> {
    let json = match target {
        ProxyTarget::System => return Ok(String::new()),
        ProxyTarget::Direct => serde_json::json!({
            "HTTP_PROXY": serde_json::Value::Null,
            "HTTPS_PROXY": serde_json::Value::Null,
            "ALL_PROXY": serde_json::Value::Null,
        }),
        ProxyTarget::Profile(p) => {
            let mut map = serde_json::Map::new();
            map.insert(
                "HTTP_PROXY".into(),
                serde_json::Value::String(p.url.clone()),
            );
            map.insert(
                "HTTPS_PROXY".into(),
                serde_json::Value::String(p.url.clone()),
            );
            map.insert("ALL_PROXY".into(), serde_json::Value::String(p.url.clone()));
            if !p.no_proxy.trim().is_empty() {
                map.insert(
                    "NO_PROXY".into(),
                    serde_json::Value::String(p.no_proxy.trim().to_owned()),
                );
            }
            serde_json::Value::Object(map)
        }
    };
    let plain =
        serde_json::to_vec(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    #[cfg(windows)]
    let cipher = dpapi_protect_no_entropy(&plain)?;
    #[cfg(not(windows))]
    let cipher = plain;
    Ok(base64_encode(&cipher))
}

// Only PowerShell handles plaintext. The workspace and the history cache carry the
// base64 of a current-user DPAPI blob, or an empty string for "inherit".
fn read_seal(path: &Path) -> io::Result<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut seal = String::new();
    file.take(2 * 1024 * 1024 + 1).read_to_string(&mut seal)?;
    if seal.len() > 2 * 1024 * 1024
        || !seal
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "会话代理记录格式无效",
        ));
    }
    Ok(Some(seal))
}

pub struct SessionProxy {
    root: PathBuf,
    pub directory: PathBuf,
    seed: Option<String>,
    owned: OnceLock<()>,
}

impl SessionProxy {
    pub fn new(root: &Path, id: SessionId, seed: Option<String>) -> Self {
        Self {
            root: root.to_owned(),
            directory: root.join("proxy-live").join(id.marker()),
            seed,
            owned: OnceLock::new(),
        }
    }
    fn history_path(&self, key: &Key) -> io::Result<PathBuf> {
        let key = Key::new(&key.agent, &key.id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "会话 ID 无效"))?;
        Ok(self
            .root
            .join("proxy-history")
            .join(format!("{}-{}.bin", key.agent, key.id)))
    }
    pub fn prepare(&self, key: Option<&Key>) -> io::Result<()> {
        let seal = match (&self.seed, key) {
            (Some(seed), _) => Some(seed.clone()),
            (None, Some(key)) => read_seal(&self.history_path(key)?)?,
            _ => None,
        };
        fs::create_dir_all(self.directory.parent().unwrap())?;
        fs::create_dir(&self.directory)?;
        self.owned.set(()).expect("one proxy exchange per shell");
        let pending = self.directory.join("initial");
        fs::write(&pending, seal.unwrap_or_default())?;
        fs::rename(pending, self.directory.join("current"))
    }
    pub fn snapshot(&self) -> io::Result<Option<String>> {
        if self.owned.get().is_none() {
            return Ok(self.seed.clone());
        }
        Ok(read_seal(&self.directory.join("current"))?.or_else(|| self.seed.clone()))
    }
    pub fn remember(&self, run: &str, key: &Key) -> io::Result<()> {
        if !crate::hook_wire::valid_run(run) {
            return Ok(());
        }
        // The shell froze this copy immediately before invoking this particular CLI.
        let Some(seal) = read_seal(&self.directory.join(run))? else {
            return Ok(());
        };
        let path = self.history_path(key)?;
        if read_seal(&path)?.as_deref().unwrap_or_default() == seal {
            return Ok(());
        }
        fs::create_dir_all(path.parent().unwrap())?;
        let pending = path.with_extension(format!("{run}.pending"));
        fs::write(&pending, seal)?;
        fs::rename(pending, path)
    }
}

impl Drop for SessionProxy {
    fn drop(&mut self) {
        if self.owned.get().is_some() {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_tabs_of_one_conversation_keep_their_own_snapshot() {
        let dir = crate::tests::phase_four::folder("proxy-same-thread");
        let key = Key::new("codex", "01234567-1234-4321-8765-000000000001").unwrap();
        let run = "01234567123443218765000000000001";
        // Storage treats the envelope as opaque. DPAPI itself is exercised by both shells.
        let history = SessionProxy::new(&dir, SessionId::new(), None);
        history.prepare(None).unwrap();
        fs::write(history.directory.join(run), "QUFB").unwrap();
        history.remember(run, &key).unwrap();
        for seed in [Some("QkJC"), Some(""), None] {
            let restored = SessionProxy::new(&dir, SessionId::new(), seed.map(str::to_owned));
            assert_eq!(restored.snapshot().unwrap().as_deref(), seed);
            restored.prepare(Some(&key)).unwrap();
            assert_eq!(
                restored.snapshot().unwrap().as_deref(),
                seed.or(Some("QUFB"))
            );
        }
        fs::write(history.directory.join(run), "").unwrap();
        history.remember(run, &key).unwrap();
        let later = SessionProxy::new(&dir, SessionId::new(), None);
        later.prepare(Some(&key)).unwrap();
        assert_eq!(later.snapshot().unwrap().as_deref(), Some(""));
    }

    #[test]
    fn proxy_profile_validation_sanitization_and_matching() {
        assert_eq!(
            normalize_proxy_url("10808").unwrap(),
            "http://127.0.0.1:10808"
        );
        assert_eq!(
            normalize_proxy_url("127.0.0.1:7890").unwrap(),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            normalize_proxy_url("socks5://127.0.0.1:10809").unwrap(),
            "socks5://127.0.0.1:10809"
        );
        assert!(normalize_proxy_url("0").is_err());
        assert!(normalize_proxy_url("ftp://127.0.0.1:21").is_err());
        assert!(normalize_profile_name("off").is_err());
        assert!(normalize_profile_name("123").is_err());
        assert!(normalize_profile_name("bad name").is_err());
        assert_eq!(
            sanitize_proxy_summary("http://alice:secret@127.0.0.1:10808"),
            "http://127.0.0.1:10808"
        );
        assert_eq!(short_host_port("http://127.0.0.1:10808"), "10808");
        assert_eq!(short_host_port("http://10.0.0.2:7890"), "10.0.0.2:7890");

        let mut settings = ProxySettings::default();
        settings
            .upsert_profile("p1", "10808", "localhost,127.0.0.1")
            .unwrap();
        settings
            .upsert_profile("p2", "socks5://127.0.0.1:7890", "")
            .unwrap();
        settings.default_profile = Some("p1".into());
        settings.set_agent_default("codex", Some("p1".into()));

        assert_eq!(
            settings.find_profile("1").map(|p| p.name.as_str()),
            Some("p1")
        );
        assert_eq!(
            settings.find_profile("P2").map(|p| p.url.as_str()),
            Some("socks5://127.0.0.1:7890")
        );
        assert_eq!(
            settings.match_summary("http://127.0.0.1:10808"),
            SummaryMatch::Profile {
                name: "p1".into(),
                url: "http://127.0.0.1:10808".into()
            }
        );
        assert_eq!(settings.match_summary("off"), SummaryMatch::Direct);
        assert_eq!(
            settings.match_summary("sys:http://127.0.0.1:9999"),
            SummaryMatch::System(Some("http://127.0.0.1:9999".into()))
        );
        assert!(settings.remove_profile("p1"));
        assert!(settings.default_profile.is_none());
        assert!(settings.agent_default("codex").is_none());
    }

    #[test]
    fn seal_target_round_trips_with_powershell_protected_data() {
        let p = ProxyProfile {
            name: "p1".into(),
            url: "http://127.0.0.1:10808".into(),
            no_proxy: "localhost".into(),
        };
        let sealed_profile = seal_target(&ProxyTarget::Profile(p)).unwrap();
        let sealed_direct = seal_target(&ProxyTarget::Direct).unwrap();
        let sealed_sys = seal_target(&ProxyTarget::System).unwrap();
        assert!(sealed_sys.is_empty());
        assert!(!sealed_profile.contains("10808"));
        assert!(sealed_direct.len() > 40);

        let dir = crate::tests::phase_four::folder("proxy-dpapi-roundtrip");
        fs::write(dir.join("current"), &sealed_profile).unwrap();
        let script = format!(
            "{}\nif ($env:HTTP_PROXY -cne 'http://127.0.0.1:10808' -or $env:HTTPS_PROXY -cne 'http://127.0.0.1:10808' -or $env:ALL_PROXY -cne 'http://127.0.0.1:10808' -or $env:NO_PROXY -cne 'localhost') {{ exit 11 }}\n",
            include_str!("shell_proxy.ps1")
        );
        let out = std::process::Command::new(crate::shell::program().unwrap())
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(&script)
            .env("MCLI_PROXY_DIR", &dir)
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("NO_PROXY")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "PowerShell failed to decrypt Rust DPAPI seal: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
