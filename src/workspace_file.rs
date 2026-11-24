use crate::{transcript::Key, workspace::Workspace};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
pub struct SavedTab {
    pub directory: PathBuf,
    pub conversation: Option<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    version: u8,
    pub tabs: Vec<SavedTab>,
    pub selected: Option<usize>,
    pub collapsed: HashSet<PathBuf>,
}

impl Snapshot {
    pub fn from_window(w: &Workspace) -> io::Result<Self> {
        let mut tabs = Vec::new();
        let mut selected = None;
        let mut collapsed = HashSet::new();
        for tab in w
            .tabs
            .iter()
            .filter(|t| !t.closing && t.directory.is_absolute())
        {
            if Some(tab.id) == w.active {
                selected = Some(tabs.len());
            }
            if w.layout.collapsed.contains(&tab.project) {
                collapsed.insert(tab.project.clone());
            }
            tabs.push(SavedTab {
                directory: tab.directory.clone(),
                conversation: tab.bound.as_ref().or(tab.expected.as_ref()).cloned(),
                proxy: tab
                    .proxy
                    .as_ref()
                    .map(|p| p.snapshot())
                    .transpose()?
                    .flatten(),
            });
        }
        Ok(Self {
            version: 1,
            tabs,
            selected,
            collapsed,
        })
    }
    fn read(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut snapshot: Self = serde_json::from_reader(io::BufReader::new(file))?;
        if snapshot.version != 1 || snapshot.tabs.iter().any(|t| !t.directory.is_absolute()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "工作台版本或目录无效",
            ));
        }
        for tab in &mut snapshot.tabs {
            tab.conversation = tab
                .conversation
                .take()
                .and_then(|k| Key::new(&k.agent, &k.id));
        }
        Ok(snapshot)
    }
}

pub struct WindowFile {
    path: PathBuf,
    // An exclusive Windows handle reserves one snapshot for the lifetime of this window.
    _lock: File,
}

impl WindowFile {
    pub fn folder(&self) -> &Path {
        self.path.parent().unwrap()
    }
    pub fn claim(folder: &Path) -> io::Result<(Self, Option<Snapshot>, Option<String>)> {
        fs::create_dir_all(folder)?;
        let mut saved = Vec::new();
        for entry in fs::read_dir(folder)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json")
                && let Some(number) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u64>().ok())
                && let Ok(modified) = path.metadata().and_then(|m| m.modified())
            {
                saved.push((modified, number));
            }
        }
        saved.sort_unstable_by(|a, b| b.cmp(a));
        let mut warning = None;
        for (_, slot) in saved {
            if let Some(owned) = Self::reserve(folder, slot)? {
                match Snapshot::read(&owned.path) {
                    Ok(snapshot) => return Ok((owned, Some(snapshot), warning)),
                    Err(e) => {
                        warning = Some(format!(
                            "工作台记录 {} 未能读取 · {e}",
                            owned.path.display()
                        ))
                    }
                }
            }
        }
        for slot in 1.. {
            if let Some(owned) = Self::reserve(folder, slot)? {
                match owned.path.try_exists()? {
                    false => return Ok((owned, None, warning)),
                    true => continue,
                }
            }
        }
        unreachable!()
    }
    fn reserve(folder: &Path, slot: u64) -> io::Result<Option<Self>> {
        let path = folder.join(format!("{slot}.json"));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .share_mode(0)
            .open(path.with_extension("lock"));
        match lock {
            Ok(lock) => Ok(Some(Self { path, _lock: lock })),
            Err(e) if matches!(e.raw_os_error(), Some(32 | 33)) => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub fn save(&self, snapshot: &Snapshot) -> io::Result<()> {
        let data = serde_json::to_vec(snapshot)?;
        let staging = self.path.with_extension("pending");
        let mut file = File::create(&staging)?;
        file.write_all(&data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(staging, &self.path)
    }
}
