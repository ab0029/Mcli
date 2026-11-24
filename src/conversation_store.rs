use crate::transcript::{self, Key, Roots, Row, invalid, next_row, snippet};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const PAGE: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub key: Key,
    pub project: PathBuf,
    pub preview: String,
    pub phase: String,
    pub count: usize,
    pub last_prompt_ms: Option<i64>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub number: usize,
    pub time: String,
    pub snippet: String,
}

pub struct SavedSource {
    pub rowid: i64,
    pub key: Key,
    pub project: PathBuf,
    pub path: PathBuf,
}

const COLUMNS: &str = "agent,id,project,preview,phase,prompt_count,CAST(round(unixepoch(last_user_time,'subsec')*1000) AS INTEGER)";
const ORDER: &str = " ORDER BY last_user_time DESC,agent,id";
fn summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        key: Key {
            agent: row.get(0)?,
            id: row.get(1)?,
        },
        project: PathBuf::from(row.get::<_, String>(2)?),
        preview: row.get(3)?,
        phase: row.get(4)?,
        count: row.get(5)?,
        last_prompt_ms: row.get(6)?,
    })
}

pub struct Store {
    conn: Connection,
    roots: Roots,
}

impl Store {
    pub fn open(path: &Path, roots: Roots) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_millis(750))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            return Err(invalid("此索引由更新版本的 Mcli 创建").into());
        }
        if version == 0 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if version == 0 {
                let occupied: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%')",
                    [],
                    |r| r.get(0),
                )?;
                if occupied {
                    return Err(invalid("索引路径已有其他数据库，请核对文件位置").into());
                }
                tx.execute_batch("
                CREATE TABLE IF NOT EXISTS conversations(
                  agent TEXT NOT NULL, id TEXT NOT NULL, project TEXT NOT NULL, source TEXT NOT NULL,
                  file_id TEXT NOT NULL DEFAULT '', cursor INTEGER NOT NULL DEFAULT 0, anchor BLOB NOT NULL DEFAULT X'',
                  modified TEXT NOT NULL DEFAULT '', preview TEXT NOT NULL DEFAULT '', candidate TEXT NOT NULL DEFAULT '',
                  phase TEXT NOT NULL DEFAULT '状态未知', last_user_time TEXT, prompt_count INTEGER NOT NULL DEFAULT 0,
                  PRIMARY KEY(agent,id));
                CREATE TABLE IF NOT EXISTS prompts(
                  agent TEXT NOT NULL,id TEXT NOT NULL,ordinal INTEGER NOT NULL,message_key TEXT NOT NULL,
                  stamp TEXT, summary TEXT NOT NULL, position INTEGER NOT NULL, length INTEGER NOT NULL, digest BLOB NOT NULL,
                  PRIMARY KEY(agent,id,ordinal), UNIQUE(agent,id,message_key),
                  FOREIGN KEY(agent,id) REFERENCES conversations(agent,id));
                CREATE INDEX IF NOT EXISTS recent_tasks ON conversations(last_user_time DESC,agent,id) WHERE prompt_count>0;
                PRAGMA user_version=1;")?;
            }
            tx.commit()?;
        }
        conn.prepare("SELECT message_key,digest FROM prompts LIMIT 0")?;
        conn.prepare("SELECT prompt_count,anchor FROM conversations LIMIT 0")?;
        Ok(Self { conn, roots })
    }
    pub fn recent(&self) -> Result<Vec<Summary>> {
        self.history(None, 10, 0)
    }
    pub fn history(
        &self,
        project: Option<&Path>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Summary>> {
        let filter = if project.is_some() {
            " AND (replace(project,'/','\\')=?3 OR substr(replace(project,'/','\\'),1,length(?4))=?4)"
        } else {
            ""
        };
        let sql = format!(
            "SELECT {COLUMNS} FROM conversations WHERE prompt_count>0{filter}{ORDER} LIMIT ?1 OFFSET ?2"
        );
        let mut st = self.conn.prepare(&sql)?;
        let values = if let Some(p) = project {
            let normalized = crate::workspace::project_key(p);
            let dir = normalized.to_string_lossy();
            let prefix = format!("{}\\", dir.trim_end_matches('\\'));
            st.query_map(params![limit, offset, dir, prefix], summary)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            st.query_map(params![limit, offset], summary)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(values)
    }
    pub fn projects(&self) -> Result<Vec<PathBuf>> {
        let mut st = self.conn.prepare(
            "SELECT DISTINCT project FROM conversations WHERE prompt_count>0 ORDER BY project",
        )?;
        Ok(st
            .query_map([], |r| Ok(PathBuf::from(r.get::<_, String>(0)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn get(&self, key: &Key) -> Result<Option<Summary>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM conversations WHERE agent=? AND id=?"),
                params![key.agent, key.id],
                summary,
            )
            .optional()?)
    }
    pub fn page(&self, key: &Key, offset: usize) -> Result<Vec<Entry>> {
        let mut st=self.conn.prepare("SELECT ordinal,coalesce(strftime('%Y-%m-%d %H:%M',stamp,'localtime'),'时间未知'),summary FROM prompts WHERE agent=? AND id=? ORDER BY ordinal DESC LIMIT ? OFFSET ?")?;
        Ok(st
            .query_map(params![key.agent, key.id, PAGE, offset], |r| {
                Ok(Entry {
                    number: r.get(0)?,
                    time: r.get(1)?,
                    snippet: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn text(&self, key: &Key, number: usize) -> Result<String> {
        let (path,offset,len,digest):(String,u64,usize,Vec<u8>)=self.conn.query_row(
            "SELECT c.source,p.position,p.length,p.digest FROM prompts p JOIN conversations c USING(agent,id) WHERE p.agent=? AND p.id=? AND p.ordinal=?",
            params![key.agent,key.id,number],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        Ok(transcript::body(
            &self.roots,
            key,
            Path::new(&path),
            offset,
            len,
            &digest,
        )?)
    }

    /// Search the official text in small descending batches. The index supplies verified
    /// offsets; the complete text is never duplicated in SQLite.
    pub fn search_batch(
        &self,
        key: &Key,
        needle: &str,
        before: i64,
        hits: &mut Vec<Entry>,
    ) -> Result<Option<i64>> {
        let Some(path) = self.source(key)? else {
            return Ok(None);
        };
        let mut reader = BufReader::new(File::open(self.roots.path(key, &path)?)?);
        let mut st = self.conn.prepare_cached(
            "SELECT ordinal,coalesce(strftime('%Y-%m-%d %H:%M',stamp,'localtime'),'时间未知'),summary,position,length,digest FROM prompts WHERE agent=? AND id=? AND ordinal<? ORDER BY ordinal DESC LIMIT 64",
        )?;
        let mut rows = st.query(params![key.agent, key.id, before])?;
        let mut last = None;
        let mut scanned = 0;
        while let Some(row) = rows.next()? {
            let ordinal: i64 = row.get(0)?;
            let digest: Vec<u8> = row.get(5)?;
            let body = transcript::body_at(&mut reader, key, row.get(3)?, row.get(4)?, &digest)?;
            if body.to_lowercase().contains(needle) {
                hits.push(Entry {
                    number: ordinal as usize,
                    time: row.get(1)?,
                    snippet: row.get(2)?,
                });
                if hits.len() > PAGE {
                    return Ok(None);
                }
            }
            scanned += 1;
            last = Some(ordinal);
        }
        Ok(if scanned == 64 { last } else { None })
    }
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    /// Read a bounded page of registered paths independently of the visible recent list.
    pub fn saved_sources_after(&self, after: i64) -> Result<VecDeque<SavedSource>> {
        let mut st = self.conn.prepare_cached(
            "SELECT rowid,agent,id,project,source FROM conversations WHERE rowid>? ORDER BY rowid LIMIT 32",
        )?;
        Ok(st
            .query_map([after], |row| {
                Ok(SavedSource {
                    rowid: row.get(0)?,
                    key: Key {
                        agent: row.get(1)?,
                        id: row.get(2)?,
                    },
                    project: PathBuf::from(row.get::<_, String>(3)?),
                    path: PathBuf::from(row.get::<_, String>(4)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn last_digest(&self, key: &Key) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT lower(hex(digest)) FROM prompts WHERE agent=? AND id=? ORDER BY ordinal DESC LIMIT 1",params![key.agent,key.id],|r|r.get(0)).optional()?)
    }
    pub fn indexed_through(&self, key: &Key, end: u64) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT cursor >= ?3 FROM conversations WHERE agent=?1 AND id=?2",
                params![key.agent, key.id, end],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }
    pub fn source(&self, key: &Key) -> Result<Option<PathBuf>> {
        let saved = self
            .conn
            .query_row(
                "SELECT source FROM conversations WHERE agent=? AND id=?",
                params![key.agent, key.id],
                |r| Ok(PathBuf::from(r.get::<_, String>(0)?)),
            )
            .optional()?;
        if saved.as_ref().is_some_and(|p| p.is_file())
            || key.agent != "codex"
            || key.id.as_bytes().get(14) != Some(&b'7')
        {
            return Ok(saved);
        }
        for dir in self.codex_partitions(&key.id)? {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if self.roots.candidate(key, &path) {
                    return Ok(Some(path));
                }
            }
        }
        Ok(None)
    }

    // Match the native title to one rollout, checking both its filename and official header.
    // Old indexed records can supply an identity if their headers predate the cwd field.
    pub fn known_codex_display(&self, prefix: &str) -> Result<Option<String>> {
        if prefix.len() != 29
            || prefix.as_bytes()[14] != b'7'
            || !crate::hook_wire::valid_id(&format!("{prefix}0000000"))
        {
            return Ok(None);
        }
        let mut st = self
            .conn
            .prepare("SELECT id FROM conversations WHERE agent='codex' AND id GLOB ? LIMIT 2")?;
        let ids = st
            .query_map([format!("{prefix}*")], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.len() > 1 {
            return Ok(None);
        }
        let mut source = None;
        for dir in self.codex_partitions(prefix)? {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for entry in entries {
                let path = entry?.path();
                let Some(stem) = path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .and_then(|s| s.strip_prefix("rollout-"))
                    .and_then(|s| s.strip_suffix(".jsonl"))
                else {
                    continue;
                };
                let Some(id) = stem.get(stem.len().saturating_sub(36)..) else {
                    continue;
                };
                if !id.starts_with(prefix) || !crate::hook_wire::valid_id(id) {
                    continue;
                }
                if ids.first().is_some_and(|known| known != id) || source.is_some() {
                    return Ok(None);
                }
                let Some(key) = Key::new("codex", id) else {
                    continue;
                };
                let verified = self.roots.path(&key, &path)?;
                source = Some((key, verified));
            }
        }
        let Some((key, path)) = source else {
            return Ok(None);
        };
        if ids.is_empty() {
            return Ok(crate::discovery::codex_header(&path, &key).map(|_| key.id));
        }
        let mut reader = BufReader::new(File::open(path)?);
        let mut line = Vec::new();
        Ok((next_row(&mut reader, &mut line)?
            && matches!(transcript::parse(&key,&line)?,Row::Header(id) if id==key.id))
        .then_some(key.id))
    }

    pub fn codex_recent_dirs(&self) -> Result<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        for offset in ["0 days", "-1 days", "+1 days"] {
            let date: String =
                self.conn
                    .query_row("SELECT strftime('%Y/%m/%d','now',?1)", [offset], |row| {
                        row.get(0)
                    })?;
            dirs.push(self.roots.codex.join("sessions").join(date));
        }
        Ok(dirs)
    }

    pub fn recent_sources(
        &self,
        agent: &str,
        project: &Path,
        limit: usize,
    ) -> Result<Vec<(Key, PathBuf)>> {
        let normalized = crate::workspace::project_key(project);
        let dir = normalized.to_string_lossy();
        let prefix = format!("{}\\", dir.trim_end_matches('\\'));
        let mut st = self.conn.prepare_cached(
            "SELECT id,source FROM conversations
             WHERE agent=?1 AND source<>''
               AND (replace(project,'/','\\')=?2 COLLATE NOCASE
                 OR substr(replace(project,'/','\\'),1,length(?3))=?3 COLLATE NOCASE)
             ORDER BY coalesce(last_user_time,'') DESC, rowid DESC
             LIMIT ?4",
        )?;
        let rows = st
            .query_map(params![agent, dir, prefix, limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    PathBuf::from(r.get::<_, String>(1)?),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, path)| Key::new(agent, &id).map(|k| (k, path)))
            .collect())
    }

    fn codex_partitions(&self, uuid: &str) -> Result<Vec<PathBuf>> {
        // UUIDv7 encodes UTC creation time. The official filename uses local time.
        let stamp = i64::from_str_radix(&uuid[..13].replace('-', ""), 16)? / 1000;
        let mut dirs = Vec::new();
        for day in [0, -1, 1] {
            let date: String = self.conn.query_row(
                "SELECT strftime('%Y/%m/%d',?1,'unixepoch')",
                [stamp + day * 86400],
                |row| row.get(0),
            )?;
            dirs.push(self.roots.codex.join("sessions").join(date));
        }
        dirs.push(self.roots.codex.join("archived_sessions"));
        Ok(dirs)
    }

    // Returns true when this transaction advanced the index. The next pass handles any remaining batch.
    pub fn sync(&mut self, key: &Key, project: &Path, source: &Path) -> Result<bool> {
        let path = self.roots.path(key, source)?;
        let mut file = File::open(&path)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let identity = format!(
            "{}-{}-{}",
            info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
        );
        let modified = format!(
            "{}-{}",
            info.ftLastWriteTime.dwHighDateTime, info.ftLastWriteTime.dwLowDateTime
        );
        let size = file.metadata()?.len();
        let unchanged: bool = self.conn.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE agent=? AND id=? AND file_id=? AND cursor=? AND modified=?)",
        )?.query_row(params![key.agent,key.id,identity,size,modified], |r| r.get(0))?;
        if unchanged {
            return Ok(false);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let registered = tx.execute(
            "INSERT OR IGNORE INTO conversations(agent,id,project,source) VALUES(?,?,?,?)",
            params![
                key.agent,
                key.id,
                project.to_string_lossy(),
                path.to_string_lossy()
            ],
        )?;
        let (old_identity,old_offset,old_anchor,old_modified,old_count):(String,u64,Vec<u8>,String,usize)=tx.query_row(
            "SELECT file_id,cursor,anchor,modified,prompt_count FROM conversations WHERE agent=? AND id=?",params![key.agent,key.id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        if identity == old_identity && size == old_offset && old_modified == modified {
            return Ok(false);
        }
        let rewind = old_offset > 0
            && (identity != old_identity
                || size < old_offset
                || file_anchor(&mut file, old_offset)? != old_anchor
                || (size == old_offset && modified != old_modified));
        let mut position = if rewind { 0 } else { old_offset };
        let mut count = if rewind { 0 } else { old_count };
        file.seek(SeekFrom::Start(position))?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut processed = 0;
        while (processed < 256 || rewind && count < old_count) && next_row(&mut reader, &mut line)?
        {
            let row = transcript::parse(key, &line)?;
            if position == 0
                && key.agent == "codex"
                && !matches!(&row,Row::Header(id) if id==&key.id)
            {
                return Err(invalid("Codex 对话编号与记录不一致").into());
            }
            match row {
                Row::Header(id) if id != key.id => {
                    return Err(invalid("记录中出现不同的对话编号").into());
                }
                Row::Prompt(msg) => {
                    let digest = blake3::hash(msg.text.as_bytes());
                    let stable = if msg.key.is_empty() {
                        format!("ordinal:{}", count + 1)
                    } else {
                        msg.key
                    };
                    let previous:Option<(usize,Vec<u8>)>=tx.query_row("SELECT ordinal,digest FROM prompts WHERE agent=? AND id=? AND message_key=?",
                        params![key.agent,key.id,stable],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                    if let Some((ord, stored)) = previous {
                        if stored != digest.as_bytes() || ord > count + 1 {
                            return Err(invalid("历史消息已变更，等待核对").into());
                        }
                        if ord == count + 1 {
                            count += 1;
                            tx.execute("UPDATE prompts SET position=?,length=? WHERE agent=? AND id=? AND ordinal=?",params![position,line.len(),key.agent,key.id,ord])?;
                        }
                    } else {
                        count += 1;
                        if count <= old_count {
                            return Err(invalid("官方历史与上次读取的顺序不一致").into());
                        }
                        let preview = snippet(&msg.text);
                        tx.execute("INSERT INTO prompts(agent,id,ordinal,message_key,stamp,summary,position,length,digest)
                            VALUES(?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ',?),?,?,?,?)",params![key.agent,key.id,count,stable,msg.time,preview,position,line.len(),digest.as_bytes().as_slice()])?;
                        tx.execute("UPDATE conversations SET preview=?,candidate='',phase='运行中',last_user_time=strftime('%Y-%m-%dT%H:%M:%fZ',?) WHERE agent=? AND id=?",
                            params![preview,msg.time,key.agent,key.id])?;
                    }
                }
                Row::Complete(reply) if !rewind || count >= old_count => {
                    let text = reply.as_deref().map(snippet);
                    tx.execute("UPDATE conversations SET preview=coalesce(?1,preview),phase=CASE WHEN ?1 IS NULL AND phase IN ('已打断','出错') THEN phase ELSE '就绪' END WHERE agent=?2 AND id=?3",params![text,key.agent,key.id])?;
                }
                Row::Reply(content) if !rewind || count >= old_count => {
                    tx.execute(
                        "UPDATE conversations SET candidate=? WHERE agent=? AND id=?",
                        params![snippet(&content), key.agent, key.id],
                    )?;
                }
                Row::Interrupted | Row::Failed if !rewind || count >= old_count => {
                    let phase = if matches!(row, Row::Failed) {
                        "出错"
                    } else {
                        "已打断"
                    };
                    tx.execute(
                        "UPDATE conversations SET phase=? WHERE agent=? AND id=?",
                        params![phase, key.agent, key.id],
                    )?;
                }
                _ => (),
            }
            processed += 1;
            position += line.len() as u64;
        }
        if rewind && count < old_count {
            return Err(invalid("记录已截断，等待官方历史补齐").into());
        }
        if processed == 0 {
            // A newly reported file can still be empty or end in its first partial line.
            if registered != 0 {
                tx.commit()?;
            }
            return Ok(false);
        }
        let anchor = file_anchor(reader.get_mut(), position)?;
        tx.execute("UPDATE conversations SET source=?,file_id=?,modified=?,cursor=?,anchor=?,prompt_count=? WHERE agent=? AND id=?",
            params![path.to_string_lossy(),identity,modified,position,anchor,count,key.agent,key.id])?;
        #[cfg(test)]
        if std::env::var("MCLI_QA_COMMIT_BOUNDARY").as_deref() == Ok("before") {
            std::process::exit(43);
        }
        tx.commit()?;
        #[cfg(test)]
        if std::env::var("MCLI_QA_COMMIT_BOUNDARY").as_deref() == Ok("after") {
            std::process::exit(43);
        }
        Ok(true)
    }

    pub fn finish_agy(&mut self, key: &Key, expected_count: usize, reason: &str) -> Result<bool> {
        let status = match reason.to_ascii_lowercase().as_str() {
            "no_tool_call" | "model_stop" => "就绪",
            "error" | "max_steps_exceeded" => "出错",
            "user_cancelled" | "user_interrupt" => "已打断",
            _ => "状态未知",
        };
        Ok(self.conn.execute("UPDATE conversations SET phase=?1,preview=CASE WHEN ?1='就绪' AND candidate<>'' THEN candidate ELSE preview END
            WHERE agent='agy' AND id=?2 AND prompt_count=?3 AND (?1<>'就绪' OR candidate<>'')
              AND (phase<>?1 OR (?1='就绪' AND preview<>candidate))",
            params![status,key.id,expected_count])?>0)
    }
}
fn file_anchor(file: &mut File, end: u64) -> std::io::Result<Vec<u8>> {
    let len = end.min(256);
    file.seek(SeekFrom::Start(end - len))?;
    let mut bytes = vec![0u8; len as usize];
    file.read_exact(&mut bytes)?;
    Ok(blake3::hash(&bytes).as_bytes().to_vec())
}
