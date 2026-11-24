use crate::{terminal::SessionId, transcript::Key};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
}
