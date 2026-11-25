use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

const MAGIC: &[u8; 8] = b"MCLICLW1";
const ENTROPY: &[u8] = b"Mcli-ClawBot-DPAPI-v1";
pub const CONTEXT_TTL_MS: u64 = 24 * 3600 * 1000;
pub const ACTIVE_WINDOW_MS: u64 = (23 * 3600 + 30 * 60) * 1000;
const HOUR_MS: u64 = 3600 * 1000;

/// Sensitive ClawBot credentials and sync cursor persisted only as DPAPI ciphertext.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthRecord {
    pub bot_token: String,
    pub ilink_bot_id: String,
    pub ilink_user_id: String,
    pub base_url: String,
    #[serde(default)]
    pub get_updates_buf: String,
    #[serde(default)]
    pub last_context_token: Option<String>,
    #[serde(default)]
    pub last_inbound_ms: Option<u64>,
    /// Bitmask of keepalive reminders already sent for the current 24h context window:
    /// bit 0 = 3h remaining, bit 1 = 2h remaining, bit 2 = 1h remaining.
    #[serde(default)]
    pub keepalive_sent_mask: u8,
}

impl AuthRecord {
    pub fn is_valid(&self) -> bool {
        !self.bot_token.trim().is_empty() && !self.ilink_user_id.trim().is_empty()
    }

    pub fn effective_base_url<'a>(&'a self, override_url: &'a str) -> &'a str {
        let ov = override_url.trim().trim_end_matches('/');
        if !ov.is_empty() && ov != crate::clawbot::DEFAULT_BASE_URL {
            return ov;
        }
        let saved = self.base_url.trim().trim_end_matches('/');
        if !saved.is_empty() {
            saved
        } else if !ov.is_empty() {
            ov
        } else {
            crate::clawbot::DEFAULT_BASE_URL
        }
    }

    pub fn active_context_token(&self, now_ms: u64) -> Option<&str> {
        let token = self
            .last_context_token
            .as_deref()
            .filter(|s| !s.trim().is_empty())?;
        let last_ms = self.last_inbound_ms?;
        if now_ms.saturating_sub(last_ms) <= ACTIVE_WINDOW_MS {
            Some(token)
        } else {
            None
        }
    }

    /// Checks whether a proactive keepalive reminder (3h / 2h / 1h before 24h expiry) is due.
    /// If multiple stages elapsed while sleeping, returns only the most urgent stage and marks
    /// earlier stages as sent so only one reminder is delivered.
    pub fn take_due_keepalive_hours(&mut self, now_ms: u64) -> Option<u8> {
        self.active_context_token(now_ms)?;
        let last_ms = self.last_inbound_ms?;
        let elapsed = now_ms.saturating_sub(last_ms);
        let remaining = CONTEXT_TTL_MS.saturating_sub(elapsed);

        // Ordered from most urgent (1h) to earliest (3h).
        const STAGES: [(u8, u64, u8); 3] = [
            (1, HOUR_MS, 0b111),     // <= 1h left -> mark 3h, 2h, 1h done
            (2, 2 * HOUR_MS, 0b011), // <= 2h left -> mark 3h, 2h done
            (3, 3 * HOUR_MS, 0b001), // <= 3h left -> mark 3h done
        ];
        for (hours_left, threshold_ms, cover_mask) in STAGES {
            let stage_bit = 1u8 << (3 - hours_left);
            if remaining <= threshold_ms {
                if (self.keepalive_sent_mask & stage_bit) == 0 {
                    self.keepalive_sent_mask |= cover_mask;
                    return Some(hours_left);
                }
                return None;
            }
        }
        None
    }
}

pub fn dpapi_protect(plaintext: &[u8]) -> io::Result<Vec<u8>> {
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(plaintext.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "凭据数据过长"))?,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            null(),
            &mut entropy,
            null_mut(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let slice = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let mut wrapped = Vec::with_capacity(MAGIC.len() + slice.len());
    wrapped.extend_from_slice(MAGIC);
    wrapped.extend_from_slice(slice);
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(wrapped)
}

pub fn dpapi_unprotect(blob: &[u8]) -> io::Result<Vec<u8>> {
    if blob.len() <= MAGIC.len() || &blob[..MAGIC.len()] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "微信凭据格式无效或已损坏",
        ));
    }
    let cipher = &blob[MAGIC.len()..];
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(cipher.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "微信凭据长度异常"))?,
        pbData: cipher.as_ptr() as *mut u8,
    };
    let mut entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            null_mut(),
            &mut entropy,
            null_mut(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("无法解密微信凭据：{}", io::Error::last_os_error()),
        ));
    }
    let plain =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(plain)
}

#[derive(Clone, Debug)]
pub struct AuthStore {
    path: PathBuf,
}

impl AuthStore {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            path: dir.join("clawbot-auth.bin"),
        }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> io::Result<Option<AuthRecord>> {
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let plain = dpapi_unprotect(&bytes)?;
        let record: AuthRecord = serde_json::from_slice(&plain).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("微信凭据解析失败：{e}"))
        })?;
        if !record.is_valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "微信凭据缺少必要字段",
            ));
        }
        Ok(Some(record))
    }

    pub fn save(&self, record: &AuthRecord) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let plain = serde_json::to_vec(record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let sealed = dpapi_protect(&plain)?;
        let tmp = self.path.with_extension("bin.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(&sealed)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &self.path).or_else(|_| {
            // Windows rename may fail if target exists on some filesystems; replace explicitly.
            let _ = fs::remove_file(&self.path);
            fs::rename(&tmp, &self.path)
        })
    }

    pub fn clear(&self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Exclusive per-machine/per-folder lock ensuring only one Mcli window polls iLink at a time.
/// Uses Windows `share_mode(0)` so the OS releases it automatically even on abrupt termination.
pub struct OwnerLock {
    _file: File,
}

impl OwnerLock {
    pub fn try_acquire(dir: &Path) -> io::Result<Option<Self>> {
        fs::create_dir_all(dir)?;
        let path = dir.join("clawbot.lock");
        match OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = file.set_len(0);
                let _ = writeln!(file, "pid={}", std::process::id());
                Ok(Some(Self { _file: file }))
            }
            Err(e)
                if e.raw_os_error() == Some(32) || e.kind() == io::ErrorKind::PermissionDenied =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mcli-clawbot-store-{label}-{}-{}",
            std::process::id(),
            now_epoch_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dpapi_store_encrypts_at_rest_and_rejects_corrupt_data() {
        let dir = temp_dir("dpapi");
        let store = AuthStore::in_dir(&dir);
        assert!(store.load().unwrap().is_none());

        let record = AuthRecord {
            bot_token: "secret_token_987654321".into(),
            ilink_bot_id: "bot_001@im.bot".into(),
            ilink_user_id: "user_abc@im.wechat".into(),
            base_url: "https://ilinkai.weixin.qq.com".into(),
            get_updates_buf: "cursor_xyz".into(),
            last_context_token: Some("ctx_123".into()),
            last_inbound_ms: Some(1_000_000),
            keepalive_sent_mask: 0,
        };
        store.save(&record).unwrap();

        let raw = fs::read(store.path()).unwrap();
        assert!(raw.starts_with(MAGIC));
        assert!(
            !raw.windows(b"secret_token_987654321".len())
                .any(|w| w == b"secret_token_987654321"),
            "bot_token must never appear in plaintext on disk"
        );
        assert_eq!(store.load().unwrap(), Some(record));

        // Corrupt bytes should return an error cleanly without panicking.
        let mut bad = raw.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        fs::write(store.path(), bad).unwrap();
        assert!(store.load().is_err());

        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn owner_lock_enforces_single_active_window_and_releases_on_drop() {
        let dir = temp_dir("lock");
        let first = OwnerLock::try_acquire(&dir).unwrap();
        assert!(first.is_some());
        let second = OwnerLock::try_acquire(&dir).unwrap();
        assert!(second.is_none(), "second window must not steal lock");
        drop(first);
        let third = OwnerLock::try_acquire(&dir).unwrap();
        assert!(third.is_some(), "lock must be acquirable after release");
        drop(third);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_token_expires_after_23_5_hours() {
        let rec = AuthRecord {
            bot_token: "t".into(),
            ilink_bot_id: "b".into(),
            ilink_user_id: "u".into(),
            base_url: String::new(),
            get_updates_buf: String::new(),
            last_context_token: Some("ctx".into()),
            last_inbound_ms: Some(1_000),
            keepalive_sent_mask: 0,
        };
        assert_eq!(
            rec.active_context_token(1_000 + ACTIVE_WINDOW_MS),
            Some("ctx")
        );
        assert_eq!(rec.active_context_token(1_000 + ACTIVE_WINDOW_MS + 1), None);
    }

    #[test]
    fn keepalive_triggers_at_3h_2h_1h_once_each_and_coalesces_after_sleep() {
        let base = 10_000u64;
        let mut rec = AuthRecord {
            bot_token: "t".into(),
            ilink_bot_id: "b".into(),
            ilink_user_id: "u".into(),
            base_url: String::new(),
            get_updates_buf: String::new(),
            last_context_token: Some("ctx".into()),
            last_inbound_ms: Some(base),
            keepalive_sent_mask: 0,
        };
        assert_eq!(rec.take_due_keepalive_hours(base + 20 * HOUR_MS), None);
        assert_eq!(rec.take_due_keepalive_hours(base + 21 * HOUR_MS), Some(3));
        assert_eq!(
            rec.take_due_keepalive_hours(base + 21 * HOUR_MS + 60_000),
            None
        );
        assert_eq!(rec.take_due_keepalive_hours(base + 22 * HOUR_MS), Some(2));
        assert_eq!(
            rec.take_due_keepalive_hours(base + 22 * HOUR_MS + 60_000),
            None
        );
        assert_eq!(rec.take_due_keepalive_hours(base + 23 * HOUR_MS), Some(1));
        assert_eq!(
            rec.take_due_keepalive_hours(base + 23 * HOUR_MS + 60_000),
            None
        );
        assert_eq!(
            rec.take_due_keepalive_hours(base + ACTIVE_WINDOW_MS + 1),
            None
        );

        // Sleep case: waking at 22.5h skips 3h and sends only 2h once, then 1h at 23h.
        rec.keepalive_sent_mask = 0;
        assert_eq!(
            rec.take_due_keepalive_hours(base + 22 * HOUR_MS + HOUR_MS / 2),
            Some(2)
        );
        assert_eq!(
            rec.take_due_keepalive_hours(base + 22 * HOUR_MS + HOUR_MS / 2 + 1000),
            None
        );
        assert_eq!(rec.take_due_keepalive_hours(base + 23 * HOUR_MS), Some(1));
    }
}
