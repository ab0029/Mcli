use crate::transcript::{self, Key, Roots};
use crate::workspace::project_key;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[derive(Clone, Debug)]
pub struct Source {
    pub key: Key,
    pub project: PathBuf,
    pub path: PathBuf,
}

#[cfg(test)]
pub fn collect(roots: &Roots) -> Vec<Source> {
    let (tx, rx) = mpsc::sync_channel(64);
    let stop = Arc::new(AtomicBool::new(false));
    let roots = roots.clone();
    std::thread::spawn(move || enumerate(roots, tx, stop));
    rx.into_iter().collect()
}

/// Parse a Windows directory or a file URI without treating two path prefixes as relatives.
fn directory(value: &str) -> Option<PathBuf> {
    let value = if let Some(uri) = value.strip_prefix("file://") {
        let mut bytes = Vec::new();
        let mut chars = uri.as_bytes().iter().copied();
        while let Some(ch) = chars.next() {
            bytes.push(if ch == b'%' {
                let hi = char::from(chars.next()?).to_digit(16)?;
                let lo = char::from(chars.next()?).to_digit(16)?;
                (hi * 16 + lo) as u8
            } else {
                ch
            });
        }
        let decoded = String::from_utf8(bytes).ok()?;
        let local = decoded
            .strip_prefix("localhost/")
            .or_else(|| decoded.strip_prefix('/'));
        match local {
            Some(drive) if drive.as_bytes().get(1) == Some(&b':') => drive.to_owned(),
            _ => format!("//{}", decoded.trim_start_matches('/')),
        }
    } else {
        value.to_owned()
    };
    let path = PathBuf::from(value);
    (path.is_absolute() && !path.components().any(|p| matches!(p, Component::ParentDir)))
        .then(|| project_key(&path))
}

pub fn rollout_key(path: &Path) -> Option<Key> {
    let name = path
        .file_name()?
        .to_str()?
        .strip_prefix("rollout-")?
        .strip_suffix(".jsonl")?;
    let id = name.get(name.len().checked_sub(36)?..)?;
    Key::new("codex", id)
}

pub fn codex_header(path: &Path, key: &Key) -> Option<PathBuf> {
    let mut line = Vec::new();
    let mut file = BufReader::new(File::open(path).ok()?);
    if !transcript::next_row(&mut file, &mut line).ok()? {
        return None;
    }
    let row: Value = serde_json::from_slice(&line).ok()?;
    let m = &row["payload"];
    if row["type"] != "session_meta"
        || m["id"].as_str()? != key.id
        || m["source"].get("subagent").is_some()
        || m["thread_source"].get("subagent").is_some()
    {
        return None;
    }
    directory(m["cwd"].as_str()?)
}

fn agy(roots: &Roots, send: &impl Fn(Source) -> bool, cancelled: &AtomicBool) {
    let Some(home) = roots.agy.parent() else {
        return;
    };
    let mut seen = HashSet::new();
    let emit = |key: Key, project: PathBuf| {
        let dir = roots.agy.join(&key.id).join(".system_generated/logs");
        let full = dir.join("transcript_full.jsonl");
        let path = if full.is_file() {
            full
        } else {
            dir.join("transcript.jsonl")
        };
        !path.is_file() || send(Source { key, project, path })
    };
    if let Ok(f) = File::open(home.join("cache/conversation_metadata.json"))
        && let Ok(meta) =
            serde_json::from_reader::<_, Value>(BufReader::new(f.take(64 * 1024 * 1024)))
        && let Some(entries) = meta["conversations"].as_object()
    {
        for (id, m) in entries {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            let Some(key) = Key::new("agy", id) else {
                continue;
            };
            let summary = &m["summary"];
            if ["Internal", "Cloud"]
                .iter()
                .any(|k| summary[k].as_bool() == Some(true))
                || m["is_internal"] == true
            {
                seen.insert(key);
                continue;
            }
            let Some(project) = summary["WorkspaceURIs"]
                .as_array()
                .and_then(|v| v.iter().filter_map(Value::as_str).find_map(directory))
            else {
                continue;
            };
            seen.insert(key.clone());
            if !emit(key, project) {
                return;
            }
        }
    }
    if let Ok(f) = File::open(home.join("history.jsonl")) {
        let mut reader = BufReader::new(f);
        let mut line = Vec::new();
        while !cancelled.load(Ordering::Relaxed)
            && transcript::next_row(&mut reader, &mut line).unwrap_or(false)
        {
            let Ok(row) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let Some(key) = row["conversationId"]
                .as_str()
                .and_then(|s| Key::new("agy", s))
            else {
                continue;
            };
            let Some(project) = row["workspace"].as_str().and_then(directory) else {
                continue;
            };
            if seen.insert(key.clone()) && !emit(key, project) {
                return;
            }
        }
    }
}

fn kimi_cwd(meta: &Value) -> Option<PathBuf> {
    meta["cwd"]
        .as_str()
        .or_else(|| meta["workDir"].as_str())
        .or_else(|| meta["custom"]["cwd"].as_str())
        .and_then(directory)
}

fn kimi(roots: &Roots, send: &impl Fn(Source) -> bool, cancelled: &AtomicBool) {
    let sessions = roots.kimi.join("sessions");
    if !sessions.is_dir() {
        return;
    }
    let mut seen = HashSet::new();
    let emit = |key: Key, project: PathBuf, path: PathBuf| {
        !path.is_file() || send(Source { key, project, path })
    };
    if let Ok(f) = File::open(sessions.join(".index-cache/scan.json"))
        && let Ok(cache) =
            serde_json::from_reader::<_, Value>(BufReader::new(f.take(32 * 1024 * 1024)))
        && let Some(entries) = cache["sessions"].as_object()
    {
        for (id, entry) in entries {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            let summary = &entry["summary"];
            if summary["archived"] == true || summary["custom"]["parent_session_id"].is_string() {
                continue;
            }
            let Some(ws) = entry["ws"]
                .as_str()
                .or_else(|| summary["workspaceId"].as_str())
            else {
                continue;
            };
            let Some(key) = Key::new("kimi", id) else {
                continue;
            };
            let Some(project) =
                kimi_cwd(summary).or_else(|| entry["primaryDir"].as_str().and_then(directory))
            else {
                continue;
            };
            let path = sessions.join(ws).join(&key.id).join("wire.jsonl");
            if seen.insert(key.clone()) && !emit(key, project, path) {
                return;
            }
        }
    }
    let Ok(workspaces) = fs::read_dir(&sessions) else {
        return;
    };
    for ws_entry in workspaces.flatten() {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        let ws_name = ws_entry.file_name();
        if ws_name.to_str().is_some_and(|n| n.starts_with('.'))
            || !ws_entry.file_type().is_ok_and(|t| t.is_dir())
        {
            continue;
        }
        let Ok(list) = fs::read_dir(ws_entry.path()) else {
            continue;
        };
        for sess_entry in list.flatten() {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            if !sess_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Some(id) = sess_entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(key) = Key::new("kimi", &id) else {
                continue;
            };
            if seen.contains(&key) {
                continue;
            }
            let sess_dir = sess_entry.path();
            let wire = sess_dir.join("wire.jsonl");
            if !wire.is_file() {
                continue;
            }
            let meta = [
                sess_dir.join("state.json"),
                sess_dir.join("session-meta/state.json"),
            ]
            .into_iter()
            .find_map(|p| {
                File::open(p)
                    .ok()
                    .and_then(|f| serde_json::from_reader::<_, Value>(BufReader::new(f)).ok())
            });
            let Some(meta) = meta else {
                continue;
            };
            if meta["archived"] == true || meta["custom"]["parent_session_id"].is_string() {
                continue;
            }
            let Some(project) = kimi_cwd(&meta) else {
                continue;
            };
            if seen.insert(key.clone()) && !emit(key, project, wire) {
                return;
            }
        }
    }
}

fn grok(roots: &Roots, send: &impl Fn(Source) -> bool, cancelled: &AtomicBool) {
    let sessions = roots.grok.join("sessions");
    let Ok(cwd_dirs) = fs::read_dir(&sessions) else {
        return;
    };
    for cwd_entry in cwd_dirs.flatten() {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        if !cwd_entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let cwd_dir = cwd_entry.path();
        let Some(project) = transcript::grok_decode_cwd_from_dirname(&cwd_dir)
            .as_deref()
            .and_then(directory)
        else {
            continue;
        };
        let Ok(list) = fs::read_dir(&cwd_dir) else {
            continue;
        };
        for sess_entry in list.flatten() {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            if !sess_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Some(id) = sess_entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(key) = Key::new("grok", &id) else {
                continue;
            };
            let updates = sess_entry.path().join("updates.jsonl");
            if updates.is_file()
                && !send(Source {
                    key,
                    project: project.clone(),
                    path: updates,
                })
            {
                return;
            }
        }
    }
}

fn enumerate(roots: Roots, channel: mpsc::SyncSender<Source>, cancelled: Arc<AtomicBool>) {
    let send = |source| channel.send(source).is_ok();
    agy(&roots, &send, &cancelled);
    kimi(&roots, &send, &cancelled);
    grok(&roots, &send, &cancelled);
    let mut stack = vec![
        (roots.codex.join("archived_sessions"), 0),
        (roots.codex.join("sessions"), 0),
    ];
    while let Some((dir, depth)) = stack.pop() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let Ok(list) = fs::read_dir(dir) else {
            continue;
        };
        let mut children = Vec::new();
        let mut files = Vec::new();
        for entry in list.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir()
                && depth < 3
                && entry
                    .metadata()
                    .is_ok_and(|m| m.file_attributes() & 0x400 == 0)
            {
                children.push((entry.path(), depth + 1));
            } else if kind.is_file() {
                let path = entry.path();
                if let Some(key) = rollout_key(&path) {
                    files.push((path, key));
                }
            }
        }
        children.sort_unstable();
        stack.extend(children);
        files.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        for (path, key) in files {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            if let Ok(verified) = roots.path(&key, &path)
                && let Some(project) = codex_header(&verified, &key)
                && !send(Source { key, project, path })
            {
                return;
            }
        }
    }
}

#[derive(Default)]
pub struct Catalog {
    pub projects: HashSet<PathBuf>,
    known: HashMap<Key, Source>,
    scheduled: HashSet<Key>,
    pub pending: VecDeque<Source>,
    scan: Option<mpsc::Receiver<Source>>,
    cancelled: Arc<AtomicBool>,
    last_scan: Option<std::time::Instant>,
}
impl Catalog {
    pub fn request(&mut self, project: PathBuf, roots: &Roots) {
        if !project.is_absolute() || !self.projects.insert(project.clone()) {
            return;
        }
        for s in self
            .known
            .values()
            .filter(|s| s.project.starts_with(&project))
        {
            if self.scheduled.insert(s.key.clone()) {
                self.pending.push_back(s.clone());
            }
        }
        if self
            .last_scan
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(30))
        {
            self.refresh(roots);
        }
    }
    pub fn refresh(&mut self, roots: &Roots) {
        if self.scan.is_none() && !self.projects.is_empty() {
            self.last_scan = Some(std::time::Instant::now());
            self.scheduled = self.pending.iter().map(|s| s.key.clone()).collect();
            let (tx, rx) = mpsc::sync_channel(32);
            self.scan = Some(rx);
            let roots = roots.clone();
            let stop = self.cancelled.clone();
            std::thread::spawn(move || enumerate(roots, tx, stop));
        }
    }
    pub fn scanning(&self) -> bool {
        self.scan.is_some() || !self.pending.is_empty()
    }
    pub fn poll(&mut self) {
        let Some(rx) = &self.scan else { return };
        for _ in 0..32 {
            match rx.try_recv() {
                Ok(s) => {
                    if self.projects.iter().any(|p| s.project.starts_with(p))
                        && self.scheduled.insert(s.key.clone())
                    {
                        self.pending.push_back(s.clone());
                    }
                    self.known.insert(s.key.clone(), s);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.scan = None;
                    break;
                }
            }
        }
    }
}
impl Drop for Catalog {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}
