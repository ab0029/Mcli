use crate::conversation_store::{Entry, Result, Store, Summary};
use crate::hook_pipe::Server;
use crate::hook_wire::{Notice, host};
use crate::shell::AgyEvent;
use crate::terminal::SessionId;
use crate::transcript::{Key, Roots};
use rune::Waker;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

pub enum Command {
    Foreground {
        marker: String,
        run: String,
        agent: String,
        project: PathBuf,
        restore: Option<Key>,
        initial: Option<Key>,
        proxy: Option<std::sync::Arc<crate::proxy::SessionProxy>>,
    },
    Leave(String),
    TuiReady(String, String),
    Agy(String, String, u64, AgyEvent),
    CodexTitle(String, String, (String, Option<String>)),
    Page(Key, usize, u64),
    Search(Key, String, i64, u64),
    CancelSearch,
    Discover(PathBuf),
    RefreshProjects,
    Text(Key, usize, u64, bool),
    LatestReply(Key, SessionId, u64),
    RecentTurns(Key, SessionId, usize, u64),
    History(Option<PathBuf>, usize, u64),
    Refresh,
    Quit,
}
pub enum Update {
    Recent(Vec<Summary>),
    Unbound(String, String),
    Bound(String, String, Summary),
    RestoredReply(String, String, Key),
    Page(Key, usize, u64, Vec<Entry>, usize),
    Search(Key, u64, Vec<Entry>, bool, Option<String>),
    Scanning(bool),
    Activity(String, String, Option<&'static str>),
    Text(Key, usize, u64, bool, std::result::Result<String, String>),
    LatestReply(SessionId, u64, std::result::Result<Option<String>, String>),
    RecentTurns(
        SessionId,
        u64,
        std::result::Result<Vec<crate::transcript::Turn>, String>,
    ),
    History(Option<PathBuf>, usize, u64, Vec<Summary>, Vec<PathBuf>),
    Error(String),
}
pub struct Service {
    pub send: Sender<Command>,
    pub receive: Receiver<Update>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Service {
    pub fn start(db: PathBuf, roots: Roots, wake: Waker) -> Self {
        let (tx, commands) = mpsc::channel();
        let (send, receive) = mpsc::channel();
        let notify = move |update| {
            if send.send(update).is_ok() {
                wake();
            }
        };
        let instance = host(&SessionId(0).marker()).unwrap().1;
        let hook_wake = tx.clone();
        let thread = std::thread::spawn(move || {
            let store = match Store::open(&db, roots.clone()) {
                Ok(s) => s,
                Err(e) => {
                    notify(Update::Error(format!("对话索引无法打开 · {e}")));
                    return;
                }
            };
            let (hooks, notices) = mpsc::sync_channel(128);
            #[cfg(test)]
            let trace =
                std::env::var_os("MCLI_TEST_HOOK_TRACE").map(|_| db.with_extension("events.jsonl"));
            let server = Server::start(&instance, move |n| {
                #[cfg(test)]
                if std::env::var_os("MCLI_QA_WITHOUT_VENDOR_HOOKS").is_some()
                    && n.event != "StatusLine"
                {
                    return;
                }
                #[cfg(test)]
                if let Some(path) = &trace
                    && let Ok(mut file) = std::fs::OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(path)
                {
                    use std::io::Write;
                    let _ = file.write_all(&n.encode());
                    let _ = file.write_all(b"\n");
                }
                if hooks.try_send(n).is_ok() {
                    let _ = hook_wake.send(Command::Refresh);
                }
            });
            if let Err(e) = &server {
                notify(Update::Error(format!("观察钩子无法连接 · {e}")));
            }
            let mut worker = Worker {
                store,
                roots,
                live: HashMap::new(),
                pending: VecDeque::new(),
                recent: Vec::new(),
                version: -1,
                notify: &notify,
                last_external: Instant::now(),
                search: None,
                catalog: Default::default(),
            };
            worker.refresh_recent();
            let mut catch_up = VecDeque::new();
            let mut after = Some(0);
            let mut scanning = false;
            loop {
                if catch_up.is_empty()
                    && let Some(rowid) = after.take()
                {
                    match worker.store.saved_sources_after(rowid) {
                        Ok(page) => {
                            after = page.back().map(|source| source.rowid);
                            catch_up = page;
                        }
                        Err(e) => notify(Update::Error(format!("读取对话记录位置失败 · {e}"))),
                    }
                }
                let wait = if !catch_up.is_empty()
                    || worker.search.is_some()
                    || !worker.catalog.pending.is_empty()
                {
                    Duration::ZERO
                } else if worker.catalog.scanning() {
                    Duration::from_millis(18)
                } else if worker.live.is_empty() {
                    Duration::from_secs(2)
                } else {
                    Duration::from_millis(220)
                };
                let first = match commands.recv_timeout(wait) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(_) => None,
                };
                let mut quitting = false;
                for c in first.into_iter().chain(commands.try_iter().take(255)) {
                    if matches!(c, Command::Quit) {
                        quitting = true;
                        break;
                    }
                    if let Err(e) = worker.command(c) {
                        notify(Update::Error(e.to_string()));
                    }
                }
                if quitting {
                    break;
                }
                for n in notices.try_iter() {
                    worker.pending.push_back((Instant::now(), n));
                }
                while worker.pending.len() > 128 {
                    worker.pending.pop_front();
                }
                worker.apply_pending();
                worker.search_step();
                let mut dirty = false;
                // The title can arrive before Codex creates its first rollout on disk.
                let mut resolved = Vec::new();
                for slot in worker.live.values_mut() {
                    if slot.bound.is_none()
                        && let Some(prefix) = &slot.codex_prefix
                        && Instant::now() >= slot.next_check
                    {
                        slot.next_check = Instant::now() + Duration::from_secs(1);
                        if let Ok(Some(id)) = worker.store.known_codex_display(prefix) {
                            resolved.push((slot.marker.clone(), slot.run.clone(), id));
                        }
                    }
                }
                for (marker, run, id) in resolved {
                    worker.display(
                        &marker,
                        &run,
                        "codex",
                        id,
                        None,
                        crate::hook_wire::timestamp_micros(),
                    );
                }
                worker.probe_unbound_slots();
                worker.catalog.poll();
                if let Some(source) = worker.catalog.pending.pop_front()
                    && worker
                        .store
                        .sync(&source.key, &source.project, &source.path)
                        .unwrap_or(false)
                {
                    dirty = true;
                    worker.catalog.pending.push_back(source);
                }
                if scanning != worker.catalog.scanning() {
                    scanning = worker.catalog.scanning();
                    notify(Update::Scanning(scanning));
                }
                if let Some(source) = catch_up.pop_front()
                    && Key::new(&source.key.agent, &source.key.id).is_some()
                    && worker
                        .store
                        .sync(&source.key, &source.project, &source.path)
                        .unwrap_or(false)
                {
                    dirty = true;
                    // Keep each transaction bounded and accept UI commands between batches.
                    catch_up.push_back(source);
                }
                for slot in worker.live.values_mut() {
                    dirty |= slot.sync(&mut worker.store, &worker.roots, &notify);
                }
                if worker.last_external.elapsed() >= Duration::from_secs(1) {
                    worker.last_external = Instant::now();
                    if let Ok(v) = worker.store.data_version() {
                        dirty |= v != worker.version;
                        worker.version = v;
                    }
                }
                if dirty {
                    worker.refresh_recent();
                }
            }
        });
        Self {
            send: tx,
            receive,
            thread: Some(thread),
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.send.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Running {
    marker: String,
    run: String,
    agent: String,
    project: PathBuf,
    bound: Option<Key>,
    displayed: Option<Key>,
    codex_prefix: Option<String>,
    retired: HashSet<Key>,
    path: Option<PathBuf>,
    changed_at: u64,
    turn: Option<String>,
    last_event: u64,
    state: Option<String>,
    stop: Option<(String, Option<String>)>,
    signature: Option<(u64, Option<SystemTime>)>,
    previous: Option<Summary>,
    next_check: Instant,
    next_probe: Instant,
    started_sys: SystemTime,
    started_ms: i64,
    tui_ready: bool,
    requested: bool,
    meter: Option<&'static str>,
    meter_at: u64,
    osc7501_active: bool,
    osc7501_at: u64,
    composer_idle_at: u64,
    restore: Option<Key>,
    initial: Option<Key>,
    proxy: Option<std::sync::Arc<crate::proxy::SessionProxy>>,
    replies: RestoredReplies,
}

// A resumed Codex TUI can report busy/idle while replaying a completed turn.
// Count only a turn observed in the official record, after initial indexing catches up.
struct RestoredReplies {
    opened_ms: i64,
    count: Option<usize>,
    pending: Option<usize>,
}
impl RestoredReplies {
    fn observe(&mut self, s: &Summary) -> bool {
        if s.count == 0 {
            return false;
        }
        let previous = self.count.replace(s.count);
        let new_prompt = previous.map_or_else(
            || s.last_prompt_ms.is_some_and(|ms| ms >= self.opened_ms),
            |count| s.count > count,
        );
        if new_prompt || s.phase == "运行中" {
            self.pending = Some(s.count);
        }
        match s.phase.as_str() {
            "就绪" => self.pending.take() == Some(s.count),
            "出错" | "已打断" => {
                self.pending = None;
                false
            }
            _ => false,
        }
    }
}
impl Running {
    fn new(
        marker: String,
        run: String,
        agent: String,
        project: PathBuf,
        restore: Option<Key>,
        initial: Option<Key>,
        proxy: Option<std::sync::Arc<crate::proxy::SessionProxy>>,
    ) -> Self {
        let now = Instant::now();
        let started_ms = (crate::hook_wire::timestamp_micros() / 1000) as i64;
        Self {
            marker,
            run,
            agent,
            project,
            bound: None,
            displayed: None,
            codex_prefix: None,
            retired: HashSet::new(),
            path: None,
            changed_at: 0,
            turn: None,
            last_event: 0,
            state: None,
            stop: None,
            signature: None,
            previous: None,
            next_check: now,
            next_probe: now + Duration::from_millis(600),
            started_sys: SystemTime::now(),
            started_ms,
            tui_ready: false,
            requested: true,
            meter: None,
            meter_at: 0,
            osc7501_active: false,
            osc7501_at: 0,
            composer_idle_at: 0,
            restore,
            initial,
            proxy,
            replies: RestoredReplies {
                opened_ms: started_ms,
                count: None,
                pending: None,
            },
        }
    }
    fn agy_program_status(&mut self, status: &'static str, sent: u64, notify: &dyn Fn(Update)) {
        if sent < self.osc7501_at {
            return;
        }
        self.osc7501_at = sent;
        self.osc7501_active = true;
        self.meter_at = sent;
        if status.is_empty() && self.meter == Some("出错") {
            return;
        }
        let status = (!status.is_empty()).then_some(status);
        if status == Some("就绪") && self.state.as_deref() == Some("运行中") {
            self.state = None;
        }
        if self.meter != status {
            self.meter = status;
            self.requested = true;
            notify(Update::Activity(
                self.marker.clone(),
                self.run.clone(),
                status,
            ));
        }
    }
    fn agy_status(&mut self, status: &'static str, sent: u64, notify: &dyn Fn(Update)) {
        if self.osc7501_active || sent < self.changed_at || sent < self.last_event {
            return;
        }
        self.last_event = sent;
        self.meter_at = sent;
        if status.is_empty() && self.meter == Some("出错") {
            return;
        }
        let status = (!status.is_empty()).then_some(status);
        if status == Some("就绪") && self.state.as_deref() == Some("运行中") {
            self.state = None;
        }
        if self.meter != status {
            self.meter = status;
            self.requested = true;
            notify(Update::Activity(
                self.marker.clone(),
                self.run.clone(),
                status,
            ));
        }
    }
    fn agy_composer_idle(&mut self, sent: u64) {
        if !self.osc7501_active && sent >= self.composer_idle_at {
            self.composer_idle_at = sent;
            self.requested = true;
        }
    }
    fn notice(&mut self, n: Notice, roots: &Roots, notify: &dyn Fn(Update)) {
        let Some(key) = Key::new(&n.provider, &n.id) else {
            return;
        };
        if n.path
            .as_ref()
            .is_some_and(|path| !roots.candidate(&key, path))
        {
            return;
        }
        if self.agent != key.agent
            || self.run != n.run
            || n.sent < self.changed_at
            || n.sent < self.last_event
        {
            return;
        }
        if n.event == "Displayed" {
            self.displayed = Some(key.clone());
        } else if self
            .codex_prefix
            .as_ref()
            .is_some_and(|prefix| !key.id.starts_with(prefix))
        {
            return;
        } else if self
            .displayed
            .as_ref()
            .is_some_and(|current| current != &key)
        {
            return;
        }
        if self.bound.as_ref() != Some(&key) {
            if !matches!(
                n.event.as_str(),
                "Displayed" | "SessionStart" | "UserPromptSubmit" | "PreInvocation"
            ) {
                return;
            }
            if self.retired.contains(&key)
                && n.event != "Displayed"
                && n.event != "SessionStart"
                && n.event != "UserPromptSubmit"
            {
                return;
            }
            if let Some(old) = self.bound.replace(key.clone()) {
                self.retired.insert(old);
            }
            self.retired.remove(&key);
            self.path = None;
            self.signature = None;
            self.state = Some("状态未知".into());
            self.stop = None;
            self.previous = None;
            if !self.osc7501_active && self.meter.take().is_some() {
                notify(Update::Activity(
                    self.marker.clone(),
                    self.run.clone(),
                    None,
                ));
            }
            self.turn = None;
            self.changed_at = n.sent;
        }
        if n.sent < self.last_event {
            return;
        }
        if n.turn.is_some()
            && self.turn.is_some()
            && n.turn != self.turn
            && !matches!(n.event.as_str(), "SessionStart" | "UserPromptSubmit")
        {
            return;
        }
        self.last_event = n.sent;
        if let Some(p) = n.path
            && roots.candidate(&key, &p)
        {
            self.path = Some(p);
        }
        if self.path.is_none() && key.agent == "agy" {
            let dir = roots.agy.join(&key.id).join(".system_generated/logs");
            let full = dir.join("transcript_full.jsonl");
            self.path = Some(if full.exists() {
                full
            } else {
                dir.join("transcript.jsonl")
            });
        } else if self.path.is_none() && key.agent == "kimi" {
            self.path = find_kimi_wire(roots, &key.id);
        } else if self.path.is_none() && key.agent == "grok" {
            self.path = find_grok_updates(roots, &self.project, &key.id);
        }
        match n.event.as_str() {
            "Displayed" => {
                self.state = None;
            }
            "SessionStart" => {
                self.state = Some("就绪".into());
            }
            "UserPromptSubmit" | "PreInvocation" => {
                self.turn = n.turn;
                self.state = if n.event == "PreInvocation" {
                    Some("运行中".into())
                } else {
                    None
                };
                self.stop = None;
            }
            // A permission hook can be resolved automatically. Only the native
            // terminal or the vendor's live state reports an actual waiting UI.
            "PermissionRequest" => (),
            "Interrupt" => {
                self.state = Some("已打断".into());
            }
            "SessionEnd" => {
                self.state = Some("已退出".into());
            }
            "Stop" => {
                self.state = None;
                if key.agent == "agy" {
                    if n.question.is_none() {
                        self.state = Some("状态未知".into());
                    }
                    self.stop = Some((n.detail, n.question));
                }
            }
            _ => (),
        }
        self.requested = true;
    }
    fn sync(&mut self, store: &mut Store, roots: &Roots, notify: &dyn Fn(Update)) -> bool {
        let Some(key) = self.bound.as_ref() else {
            return false;
        };
        if !self.requested && Instant::now() < self.next_check {
            return false;
        }
        self.next_check = Instant::now()
            + if self.path.is_none() {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(400)
            };
        let forced = std::mem::take(&mut self.requested);
        let mut dirty = false;
        let mut error = None;
        if self.path.is_none() {
            self.path = store.source(key).ok().flatten().or_else(|| {
                let candidate = match key.agent.as_str() {
                    "kimi" => find_kimi_wire(roots, &key.id),
                    "grok" => find_grok_updates(roots, &self.project, &key.id),
                    _ => None,
                };
                candidate.and_then(|p| roots.path(key, &p).ok())
            });
        }
        if let Some(path) = &self.path {
            let signature = path.metadata().ok().map(|m| (m.len(), m.modified().ok()));
            if forced || self.signature != signature || signature.is_none() {
                match store.sync(key, &self.project, path) {
                    Ok(advanced) => {
                        dirty = advanced;
                        self.signature = signature;
                        self.requested = advanced;
                    }
                    Err(e) => {
                        error = Some(format!("待同步 · {e}"));
                        self.signature = None;
                    }
                }
            } else if self.previous.is_some() {
                return false;
            }
        }
        let summary = store.get(key).ok().flatten();
        // Without OSC 7501, agy's idle notification belongs to the turn that just finished.
        // When OSC 7501 is active, this terminal's own live state stays authoritative even if
        // another terminal writes prompts to the same conversation transcript.
        if !self.osc7501_active
            && self.meter.is_some()
            && let Some(s) = &summary
            && match s.last_prompt_ms {
                Some(stamp) => stamp > (self.meter_at / 1000) as i64,
                None => self
                    .previous
                    .as_ref()
                    .is_some_and(|old| old.count > 0 && s.count > old.count),
            }
        {
            self.meter = None;
            notify(Update::Activity(
                self.marker.clone(),
                self.run.clone(),
                None,
            ));
        }
        if key.agent == "agy"
            && let Some(s) = &summary
            && matches!(s.phase.as_str(), "运行中" | "就绪")
        {
            if self.meter == Some("就绪") {
                dirty |= store
                    .finish_agy(key, s.count, "model_stop")
                    .unwrap_or(false);
            } else if self.meter == Some("出错") && s.phase == "运行中" {
                dirty |= store.finish_agy(key, s.count, "error").unwrap_or(false);
            }
        }
        if let (Some((reason, expected)), Some(s)) = (&self.stop, &summary) {
            if expected.is_some() && store.last_digest(key).ok().flatten() == *expected {
                dirty |= store.finish_agy(key, s.count, reason).unwrap_or(false);
            }
        }
        let summary = if dirty {
            store.get(key).ok().flatten()
        } else {
            summary
        };
        let restored_reply = self.restore.as_ref() == Some(key)
            && key.agent == "codex"
            && error.is_none()
            && self
                .signature
                .is_some_and(|(end, _)| store.indexed_through(key, end).unwrap_or(false))
            && summary.as_ref().is_some_and(|s| self.replies.observe(s));
        let finished = summary.as_ref().filter(|s| {
            matches!(s.phase.as_str(), "出错" | "已打断")
                && !matches!(self.meter, Some("运行中" | "等待用户"))
        });
        let fallback_idle = (!self.osc7501_active
            && self.meter.is_none()
            && self.state.is_none()
            && self.composer_idle_at > 0
            && summary.as_ref().is_some_and(|s| {
                s.phase == "运行中"
                    && s.last_prompt_ms
                        .is_none_or(|ms| (self.composer_idle_at / 1000) as i64 > ms)
            }))
        .then(|| "就绪".to_owned());
        let phase = error
            .or_else(|| finished.map(|s| s.phase.clone()))
            .or_else(|| self.meter.map(str::to_owned))
            .or_else(|| self.state.clone())
            .or(fallback_idle)
            .or_else(|| summary.as_ref().map(|s| s.phase.clone()))
            .unwrap_or_else(|| "状态未知".into());
        let mut value = summary.unwrap_or_else(|| Summary {
            key: key.clone(),
            project: self.project.clone(),
            phase: String::new(),
            preview: String::new(),
            count: 0,
            last_prompt_ms: None,
        });
        value.phase = phase;
        if self.previous.as_ref() != Some(&value) {
            // Reopening an older tab must not replace another launch's history settings.
            if self.previous.as_ref().is_none_or(|s| &s.key != key)
                && self.restore.as_ref() != Some(key)
                && let Some(proxy) = &self.proxy
                && let Err(e) = proxy.remember(&self.run, key)
            {
                notify(Update::Error(format!("未能保存历史会话代理 · {e}")));
            }
            notify(Update::Bound(
                self.marker.clone(),
                self.run.clone(),
                value.clone(),
            ));
            self.previous = Some(value);
        }
        if restored_reply {
            notify(Update::RestoredReply(
                self.marker.clone(),
                self.run.clone(),
                key.clone(),
            ));
        }
        dirty
    }
}
struct Worker<'a> {
    store: Store,
    roots: Roots,
    live: HashMap<String, Running>,
    pending: VecDeque<(Instant, Notice)>,
    recent: Vec<Summary>,
    version: i64,
    notify: &'a dyn Fn(Update),
    last_external: Instant,
    search: Option<Search>,
    catalog: crate::discovery::Catalog,
}
struct Search {
    key: Key,
    needle: String,
    before: i64,
    generation: u64,
    hits: Vec<Entry>,
}
impl Worker<'_> {
    fn search_step(&mut self) {
        let Some(mut job) = self.search.take() else {
            return;
        };
        let old = job.hits.len();
        match self
            .store
            .search_batch(&job.key, &job.needle, job.before, &mut job.hits)
        {
            Ok(Some(before)) => {
                job.before = before;
                if old != job.hits.len() {
                    (self.notify)(Update::Search(
                        job.key.clone(),
                        job.generation,
                        job.hits.clone(),
                        false,
                        None,
                    ));
                }
                self.search = Some(job);
            }
            Ok(None) => (self.notify)(Update::Search(
                job.key,
                job.generation,
                job.hits,
                true,
                None,
            )),
            Err(e) => (self.notify)(Update::Search(
                job.key,
                job.generation,
                job.hits,
                true,
                Some(e.to_string()),
            )),
        }
    }
    fn refresh_recent(&mut self) {
        if let Ok(items) = self.store.recent()
            && self.recent != items
        {
            self.recent = items.clone();
            (self.notify)(Update::Recent(items));
        }
    }
    fn apply_pending(&mut self) {
        let mut pending = std::mem::take(&mut self.pending);
        while let Some((at, n)) = pending.pop_front() {
            if let Some(slot) = self.live.get_mut(&n.marker)
                && slot.run == n.run
            {
                if n.event == "StatusLine" {
                    if slot.agent != "agy" || n.sent < slot.changed_at || n.sent < slot.last_event {
                        continue;
                    }
                    let status = match n.detail.as_str() {
                        "waiting" => "等待用户",
                        "ready" => "就绪",
                        "working" => "运行中",
                        "error" => "出错",
                        "online" => "在线",
                        _ => continue,
                    };
                    if !n.id.is_empty() {
                        let Some(key) = Key::new("agy", &n.id) else {
                            continue;
                        };
                        if slot.bound.as_ref() != Some(&key) {
                            if slot.retired.contains(&key) {
                                continue;
                            }
                            slot.notice(
                                Notice {
                                    event: "Displayed".into(),
                                    ..n.clone()
                                },
                                &self.roots,
                                self.notify,
                            );
                        }
                    } else if slot.bound.is_some() {
                        continue;
                    }
                    slot.agy_status(status, n.sent, self.notify);
                } else {
                    slot.notice(n, &self.roots, self.notify);
                }
            } else if at.elapsed() < Duration::from_secs(3) {
                self.pending.push_back((at, n));
            }
        }
    }
    fn command(&mut self, c: Command) -> Result<()> {
        match c {
            Command::Foreground {
                marker,
                run,
                agent,
                project,
                restore,
                initial,
                proxy,
            } => {
                let immediate = restore
                    .is_none()
                    .then(|| initial.as_ref().filter(|k| k.agent == agent).cloned())
                    .flatten();
                self.live.insert(
                    marker.clone(),
                    Running::new(
                        marker.clone(),
                        run.clone(),
                        agent.clone(),
                        project,
                        restore,
                        initial,
                        proxy,
                    ),
                );
                if let Some(key) = immediate {
                    self.display(
                        &marker,
                        &run,
                        &agent,
                        key.id,
                        None,
                        crate::hook_wire::timestamp_micros(),
                    );
                }
            }
            Command::Leave(marker) => {
                if let Some(mut old) = self.live.remove(&marker) {
                    old.requested = true;
                    old.sync(&mut self.store, &self.roots, self.notify);
                }
            }
            Command::TuiReady(marker, run) => {
                let target = self
                    .live
                    .get_mut(&marker)
                    .filter(|slot| slot.run == run)
                    .and_then(|slot| {
                        slot.tui_ready = true;
                        if slot.bound.is_some() {
                            return None;
                        }
                        slot.initial
                            .as_ref()
                            .or(slot.restore.as_ref())
                            .filter(|k| k.agent == slot.agent && !slot.retired.contains(*k))
                            .map(|k| (slot.agent.clone(), k.id.clone()))
                    });
                if let Some((agent, id)) = target {
                    self.display(
                        &marker,
                        &run,
                        &agent,
                        id,
                        None,
                        crate::hook_wire::timestamp_micros(),
                    );
                }
            }
            Command::Agy(marker, run, sent, AgyEvent::Thread(Some(id))) => {
                self.display(&marker, &run, "agy", id, None, sent)
            }
            Command::Agy(marker, run, sent, AgyEvent::Status(status)) => {
                if let Some(slot) = self.live.get_mut(&marker)
                    && slot.run == run
                    && slot.agent == "agy"
                {
                    slot.agy_status(status, sent, self.notify);
                }
            }
            Command::Agy(marker, run, sent, AgyEvent::ProgramStatus(status)) => {
                if let Some(slot) = self.live.get_mut(&marker)
                    && slot.run == run
                    && slot.agent == "agy"
                {
                    slot.agy_program_status(status, sent, self.notify);
                }
            }
            Command::Agy(marker, run, sent, AgyEvent::ComposerIdle) => {
                if let Some(slot) = self.live.get_mut(&marker)
                    && slot.run == run
                    && slot.agent == "agy"
                {
                    slot.agy_composer_idle(sent);
                }
            }
            Command::Agy(marker, run, sent, AgyEvent::Thread(None)) => {
                if let Some(slot) = self.live.get_mut(&marker)
                    && slot.run == run
                    && slot.agent == "agy"
                    && sent >= slot.changed_at
                {
                    if let Some(old) = slot.bound.take() {
                        slot.retired.insert(old);
                    }
                    slot.displayed = None;
                    slot.previous = None;
                    slot.path = None;
                    if !slot.osc7501_active {
                        slot.meter = None;
                    }
                    slot.stop = None;
                    slot.changed_at = sent;
                    (self.notify)(Update::Unbound(marker, run));
                }
            }
            Command::CodexTitle(marker, run, (prefix, id)) => {
                if let Some(slot) = self.live.get_mut(&marker)
                    && slot.run == run
                    && slot.agent == "codex"
                {
                    slot.codex_prefix = Some(prefix.clone());
                    let resolved = match id {
                        Some(full) => Ok(Some(full)),
                        None => self.store.known_codex_display(&prefix),
                    };
                    if let Ok(Some(id)) = resolved {
                        self.display(
                            &marker,
                            &run,
                            "codex",
                            id,
                            None,
                            crate::hook_wire::timestamp_micros(),
                        );
                    } else {
                        slot.displayed = None;
                        if let Some(old) = slot.bound.take() {
                            slot.retired.insert(old);
                            slot.previous = None;
                            slot.path = None;
                            slot.stop = None;
                            (self.notify)(Update::Unbound(marker, run));
                        }
                        resolved?;
                    }
                }
            }
            Command::Page(k, off, generation) => {
                let entries = self.store.page(&k, off)?;
                let count = self.store.get(&k)?.map_or(0, |s| s.count);
                (self.notify)(Update::Page(k, off, generation, entries, count));
            }
            Command::Search(key, needle, before, generation) => {
                self.search = Some(Search {
                    key,
                    needle: needle.to_lowercase(),
                    before,
                    generation,
                    hits: Vec::new(),
                });
            }
            Command::CancelSearch => self.search = None,
            Command::Discover(project) => self.catalog.request(project, &self.roots),
            Command::RefreshProjects => self.catalog.refresh(&self.roots),
            Command::Text(k, ord, generation, copy) => {
                let text = self
                    .store
                    .text(&k, ord)
                    .map_err(|e| format!("原文不可用 · {e}"));
                (self.notify)(Update::Text(k, ord, generation, copy, text));
            }
            Command::LatestReply(k, tab_id, generation) => {
                for slot in self.live.values_mut() {
                    if slot.bound.as_ref() == Some(&k) {
                        slot.requested = true;
                        slot.sync(&mut self.store, &self.roots, self.notify);
                    }
                }
                let reply = self
                    .store
                    .latest_reply(&k)
                    .map_err(|e| format!("回复提取失败 · {e}"));
                (self.notify)(Update::LatestReply(tab_id, generation, reply));
            }
            Command::RecentTurns(k, tab_id, limit, generation) => {
                for slot in self.live.values_mut() {
                    if slot.bound.as_ref() == Some(&k) {
                        slot.requested = true;
                        slot.sync(&mut self.store, &self.roots, self.notify);
                    }
                }
                let turns = self
                    .store
                    .recent_turns(&k, limit)
                    .map_err(|e| format!("对话记录读取失败 · {e}"));
                (self.notify)(Update::RecentTurns(tab_id, generation, turns));
            }
            Command::History(p, off, generation) => {
                let rows = match &p {
                    Some(project) => self.store.history(
                        Some(project),
                        crate::conversation_store::PAGE + 1,
                        off,
                    )?,
                    None => Vec::new(),
                };
                let mut groups = self.store.projects()?;
                let parents: Vec<_> = self
                    .catalog
                    .projects
                    .iter()
                    .filter(|p| !groups.contains(p) && groups.iter().any(|dir| dir.starts_with(p)))
                    .cloned()
                    .collect();
                groups.extend(parents);
                groups.sort();
                (self.notify)(Update::History(p, off, generation, rows, groups));
            }
            Command::Refresh | Command::Quit => (),
        }
        Ok(())
    }

    fn display(
        &mut self,
        marker: &str,
        run: &str,
        provider: &str,
        id: String,
        path: Option<PathBuf>,
        sent: u64,
    ) {
        if let Some(slot) = self.live.get_mut(marker)
            && slot.run == run
            && slot.agent == provider
        {
            slot.notice(
                Notice {
                    marker: marker.into(),
                    run: run.into(),
                    id,
                    provider: provider.into(),
                    event: "Displayed".into(),
                    path,
                    turn: None,
                    detail: String::new(),
                    sent,
                    question: None,
                },
                &self.roots,
                self.notify,
            );
        }
    }

    // Bind unbound live CLIs passively without rewriting user command lines:
    // 1) App-restored tabs bind once the interactive TUI renders (or its official log updates).
    // 2) Fresh Codex/agy launches match newly created or updated official logs.
    fn probe_unbound_slots(&mut self) {
        const PROBE_INTERVAL: Duration = Duration::from_secs(1);
        let now = Instant::now();
        let mut claimed: HashSet<Key> = self
            .live
            .values()
            .filter_map(|slot| slot.bound.clone())
            .collect();
        let mut matches = Vec::new();
        for slot in self.live.values_mut() {
            if slot.bound.is_some() {
                continue;
            }
            let explicit = slot
                .initial
                .as_ref()
                .or(slot.restore.as_ref())
                .filter(|k| k.agent == slot.agent && !slot.retired.contains(*k));
            if let Some(key) = explicit {
                let ready = slot.restore.is_none() || slot.tui_ready;
                if ready {
                    claimed.insert(key.clone());
                    matches.push((
                        slot.marker.clone(),
                        slot.run.clone(),
                        slot.agent.clone(),
                        key.id.clone(),
                        None,
                    ));
                } else if now >= slot.next_probe {
                    slot.next_probe = now + PROBE_INTERVAL;
                    let modified = self
                        .store
                        .source(key)
                        .ok()
                        .flatten()
                        .and_then(|p| p.metadata().ok())
                        .and_then(|m| m.modified().ok())
                        .is_some_and(|t| t >= slot.started_sys);
                    if modified {
                        claimed.insert(key.clone());
                        matches.push((
                            slot.marker.clone(),
                            slot.run.clone(),
                            slot.agent.clone(),
                            key.id.clone(),
                            None,
                        ));
                    }
                }
                continue;
            }
            if now < slot.next_probe {
                continue;
            }
            slot.next_probe = now + PROBE_INTERVAL;
            let found = if slot.agent == "codex" {
                probe_codex(slot, &claimed, &self.roots, &self.store)
            } else if slot.agent == "agy" {
                probe_agy(slot, &claimed, &self.roots, &self.store)
            } else if slot.agent == "kimi" {
                probe_kimi(slot, &claimed, &self.roots, &self.store)
            } else if slot.agent == "grok" {
                probe_grok(slot, &claimed, &self.roots, &self.store)
            } else {
                None
            };
            if let Some((key, path)) = found {
                claimed.insert(key.clone());
                matches.push((
                    slot.marker.clone(),
                    slot.run.clone(),
                    slot.agent.clone(),
                    key.id,
                    path,
                ));
            }
        }
        let sent = crate::hook_wire::timestamp_micros();
        for (marker, run, agent, id, path) in matches {
            self.display(&marker, &run, &agent, id, path, sent);
        }
    }
}

fn same_project(a: &std::path::Path, b: &std::path::Path) -> bool {
    let ka = crate::workspace::project_key(a);
    let kb = crate::workspace::project_key(b);
    ka == kb
        || ka
            .to_string_lossy()
            .eq_ignore_ascii_case(&kb.to_string_lossy())
}

fn probe_codex(
    slot: &Running,
    claimed: &HashSet<Key>,
    roots: &Roots,
    store: &Store,
) -> Option<(Key, Option<PathBuf>)> {
    let min_new = slot
        .started_sys
        .checked_sub(Duration::from_secs(2))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut best: Option<(SystemTime, Key, PathBuf)> = None;
    let mut consider = |mtime: SystemTime, key: Key, path: PathBuf| {
        if best.as_ref().is_none_or(|(t, _, _)| mtime > *t) {
            best = Some((mtime, key, path));
        }
    };
    if let Ok(dirs) = store.codex_recent_dirs() {
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(key) = crate::discovery::rollout_key(&path) else {
                    continue;
                };
                if claimed.contains(&key) || slot.retired.contains(&key) {
                    continue;
                }
                let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
                    continue;
                };
                if mtime < min_new {
                    continue;
                }
                let Ok(verified) = roots.path(&key, &path) else {
                    continue;
                };
                if crate::discovery::codex_header(&verified, &key)
                    .is_some_and(|proj| same_project(&proj, &slot.project))
                {
                    consider(mtime, key, verified);
                }
            }
        }
    }
    if let Ok(known) = store.recent_sources("codex", &slot.project, 12) {
        for (key, path) in known {
            if claimed.contains(&key) || slot.retired.contains(&key) {
                continue;
            }
            if let Ok(mtime) = path.metadata().and_then(|m| m.modified())
                && mtime >= slot.started_sys
                && let Ok(verified) = roots.path(&key, &path)
            {
                consider(mtime, key, verified);
            }
        }
    }
    best.map(|(_, key, path)| (key, Some(path)))
}

fn probe_agy(
    slot: &Running,
    claimed: &HashSet<Key>,
    roots: &Roots,
    store: &Store,
) -> Option<(Key, Option<PathBuf>)> {
    use std::io::{Read, Seek, SeekFrom};
    let min_ms = slot.started_ms.saturating_sub(2_000);
    if let Some(home) = roots.agy.parent()
        && let Ok(mut file) = std::fs::File::open(home.join("history.jsonl"))
        && let Ok(len) = file.metadata().map(|m| m.len())
    {
        const TAIL: u64 = 64 * 1024;
        let offset = len.saturating_sub(TAIL);
        if file.seek(SeekFrom::Start(offset)).is_ok() {
            let mut buf = Vec::new();
            if file.read_to_end(&mut buf).is_ok() {
                let text = String::from_utf8_lossy(&buf);
                let mut best: Option<(i64, Key)> = None;
                for line in text.lines().rev() {
                    let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
                        continue;
                    };
                    let Some(ts) = row["timestamp"].as_i64() else {
                        continue;
                    };
                    if ts < min_ms {
                        continue;
                    }
                    let Some(ws) = row["workspace"].as_str() else {
                        continue;
                    };
                    if !same_project(std::path::Path::new(ws), &slot.project) {
                        continue;
                    }
                    let Some(key) = row["conversationId"]
                        .as_str()
                        .and_then(|id| Key::new("agy", id))
                    else {
                        continue;
                    };
                    if claimed.contains(&key) || slot.retired.contains(&key) {
                        continue;
                    }
                    if best.as_ref().is_none_or(|(t, _)| ts > *t) {
                        best = Some((ts, key));
                    }
                }
                if let Some((_, key)) = best {
                    return Some((key, None));
                }
            }
        }
    }
    let mut best: Option<(SystemTime, Key, PathBuf)> = None;
    if let Ok(known) = store.recent_sources("agy", &slot.project, 12) {
        for (key, path) in known {
            if claimed.contains(&key) || slot.retired.contains(&key) {
                continue;
            }
            if let Ok(mtime) = path.metadata().and_then(|m| m.modified())
                && mtime >= slot.started_sys
                && let Ok(verified) = roots.path(&key, &path)
                && best.as_ref().is_none_or(|(t, _, _)| mtime > *t)
            {
                best = Some((mtime, key, verified));
            }
        }
    }
    best.map(|(_, key, path)| (key, Some(path)))
}

fn find_kimi_wire(roots: &Roots, id: &str) -> Option<PathBuf> {
    let sessions = roots.kimi.join("sessions");
    for ws in std::fs::read_dir(&sessions).ok()?.flatten() {
        if !ws.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let wire = ws.path().join(id).join("wire.jsonl");
        if wire.is_file() {
            return Some(wire);
        }
    }
    None
}

fn find_grok_updates(roots: &Roots, project: &Path, id: &str) -> Option<PathBuf> {
    let sessions = roots.grok.join("sessions");
    if let Some(proj) = project.to_str() {
        let direct = sessions
            .join(crate::transcript::grok_encode_cwd_dirname(proj))
            .join(id)
            .join("updates.jsonl");
        if direct.is_file() {
            return Some(direct);
        }
    }
    for cwd_dir in std::fs::read_dir(&sessions).ok()?.flatten() {
        if !cwd_dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let updates = cwd_dir.path().join(id).join("updates.jsonl");
        if updates.is_file() {
            return Some(updates);
        }
    }
    None
}

fn probe_kimi(
    slot: &Running,
    claimed: &HashSet<Key>,
    roots: &Roots,
    store: &Store,
) -> Option<(Key, Option<PathBuf>)> {
    let min_new = slot
        .started_sys
        .checked_sub(Duration::from_secs(2))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut best: Option<(SystemTime, Key, PathBuf)> = None;
    let mut consider = |mtime: SystemTime, key: Key, path: PathBuf| {
        if best.as_ref().is_none_or(|(t, _, _)| mtime > *t) {
            best = Some((mtime, key, path));
        }
    };
    let sessions = roots.kimi.join("sessions");
    if let Ok(workspaces) = std::fs::read_dir(&sessions) {
        for ws in workspaces.flatten() {
            if ws.file_name().to_str().is_some_and(|n| n.starts_with('.'))
                || !ws.file_type().is_ok_and(|t| t.is_dir())
            {
                continue;
            }
            let Ok(list) = std::fs::read_dir(ws.path()) else {
                continue;
            };
            for sess in list.flatten() {
                if !sess.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let Some(id) = sess.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(key) = Key::new("kimi", &id) else {
                    continue;
                };
                if claimed.contains(&key) || slot.retired.contains(&key) {
                    continue;
                }
                let dir = sess.path();
                let wire = dir.join("wire.jsonl");
                let Ok(mtime) = wire.metadata().and_then(|m| m.modified()) else {
                    continue;
                };
                if mtime < min_new {
                    continue;
                }
                let meta = [dir.join("state.json"), dir.join("session-meta/state.json")]
                    .into_iter()
                    .find_map(|p| {
                        std::fs::File::open(p)
                            .ok()
                            .and_then(|f| serde_json::from_reader::<_, serde_json::Value>(f).ok())
                    });
                let Some(meta) = meta else {
                    continue;
                };
                if meta["archived"] == true || meta["custom"]["parent_session_id"].is_string() {
                    continue;
                }
                let cwd = meta["cwd"]
                    .as_str()
                    .or_else(|| meta["workDir"].as_str())
                    .or_else(|| meta["custom"]["cwd"].as_str());
                if cwd.is_some_and(|c| same_project(Path::new(c), &slot.project))
                    && let Ok(verified) = roots.path(&key, &wire)
                {
                    consider(mtime, key, verified);
                }
            }
        }
    }
    if let Ok(known) = store.recent_sources("kimi", &slot.project, 12) {
        for (key, path) in known {
            if claimed.contains(&key) || slot.retired.contains(&key) {
                continue;
            }
            if let Ok(mtime) = path.metadata().and_then(|m| m.modified())
                && mtime >= slot.started_sys
                && let Ok(verified) = roots.path(&key, &path)
            {
                consider(mtime, key, verified);
            }
        }
    }
    best.map(|(_, key, path)| (key, Some(path)))
}

fn probe_grok(
    slot: &Running,
    claimed: &HashSet<Key>,
    roots: &Roots,
    store: &Store,
) -> Option<(Key, Option<PathBuf>)> {
    let min_new = slot
        .started_sys
        .checked_sub(Duration::from_secs(3))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    // Prefer Grok Build's live ~/.grok/active_sessions.json registry when available.
    if let Ok(bytes) = std::fs::read(roots.grok.join("active_sessions.json"))
        && let Ok(entries) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes)
    {
        let mut best: Option<(String, Key, Option<PathBuf>)> = None;
        for entry in entries {
            let Some(id) = entry["session_id"].as_str() else {
                continue;
            };
            let Some(cwd) = entry["cwd"].as_str() else {
                continue;
            };
            if !same_project(Path::new(cwd), &slot.project) {
                continue;
            }
            let Some(key) = Key::new("grok", id) else {
                continue;
            };
            if claimed.contains(&key) || slot.retired.contains(&key) {
                continue;
            }
            let opened = entry["opened_at"].as_str().unwrap_or("").to_owned();
            let path = find_grok_updates(roots, Path::new(cwd), &key.id)
                .and_then(|p| roots.path(&key, &p).ok());
            if best.as_ref().is_none_or(|(t, _, _)| opened > *t) {
                best = Some((opened, key, path));
            }
        }
        if let Some((_, key, path)) = best {
            return Some((key, path));
        }
    }
    let mut best: Option<(SystemTime, Key, PathBuf)> = None;
    let mut consider = |mtime: SystemTime, key: Key, path: PathBuf| {
        if best.as_ref().is_none_or(|(t, _, _)| mtime > *t) {
            best = Some((mtime, key, path));
        }
    };
    let sessions = roots.grok.join("sessions");
    if let Ok(cwd_dirs) = std::fs::read_dir(&sessions) {
        for cwd_entry in cwd_dirs.flatten() {
            if !cwd_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let cwd_dir = cwd_entry.path();
            let Some(cwd) = crate::transcript::grok_decode_cwd_from_dirname(&cwd_dir) else {
                continue;
            };
            if !same_project(Path::new(&cwd), &slot.project) {
                continue;
            }
            let Ok(list) = std::fs::read_dir(&cwd_dir) else {
                continue;
            };
            for sess in list.flatten() {
                if !sess.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let Some(id) = sess.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(key) = Key::new("grok", &id) else {
                    continue;
                };
                if claimed.contains(&key) || slot.retired.contains(&key) {
                    continue;
                }
                let updates = sess.path().join("updates.jsonl");
                let Ok(mtime) = updates.metadata().and_then(|m| m.modified()) else {
                    continue;
                };
                if mtime >= min_new
                    && let Ok(verified) = roots.path(&key, &updates)
                {
                    consider(mtime, key, verified);
                }
            }
        }
    }
    if let Ok(known) = store.recent_sources("grok", &slot.project, 12) {
        for (key, path) in known {
            if claimed.contains(&key) || slot.retired.contains(&key) {
                continue;
            }
            if let Ok(mtime) = path.metadata().and_then(|m| m.modified())
                && mtime >= slot.started_sys
                && let Ok(verified) = roots.path(&key, &path)
            {
                consider(mtime, key, verified);
            }
        }
    }
    best.map(|(_, key, path)| (key, Some(path)))
}
