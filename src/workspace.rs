use crate::proxy::SessionProxy;
use crate::{
    shell,
    terminal::{End, Session, SessionId, Size},
    terminal_view::Screen,
};
use rune::{Tx, Waker};
use std::collections::VecDeque;
use std::io;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::Arc;
use std::sync::{atomic::Ordering, mpsc};

type Startup = mpsc::Receiver<Result<Session, String>>;

struct ShellLaunch {
    id: SessionId,
    cwd: io::Result<PathBuf>,
    resume: Option<crate::transcript::Key>,
    proxy: Option<Arc<SessionProxy>>,
    proxy_catalog: Option<PathBuf>,
    send: mpsc::Sender<Result<Session, String>>,
}
impl ShellLaunch {
    fn start(self, size: Size, wake: Waker) {
        std::thread::spawn(move || {
            let result = self
                .cwd
                .and_then(|cwd| {
                    if let Some(proxy) = &self.proxy {
                        proxy.prepare(self.resume.as_ref())?;
                    }
                    Session::spawn_with_proxy(
                        self.id,
                        &shell::program()?,
                        &shell::session_arguments(self.resume.as_ref(), self.proxy.is_some()),
                        &cwd,
                        size,
                        wake.clone(),
                        self.proxy.as_ref().map(|p| p.directory.as_path()),
                        self.proxy_catalog.as_deref(),
                    )
                })
                .map_err(|e| e.to_string());
            let _ = self.send.send(result);
            wake();
        });
    }
}

// Preserve directory component case. On Windows a directory can be case-sensitive.
pub(super) fn project_key(path: &Path) -> PathBuf {
    let mut parts = path.components();
    let mut key = match parts.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                PathBuf::from(format!("{}:\\", char::from(letter.to_ascii_uppercase())))
            }
            Prefix::UNC(host, share) | Prefix::VerbatimUNC(host, share) => {
                let mut p = PathBuf::from(format!(
                    r"\\{}",
                    host.to_string_lossy().to_ascii_lowercase()
                ));
                p.push(share.to_string_lossy().to_ascii_lowercase());
                p
            }
            _ => return path.to_owned(),
        },
        _ => return path.to_owned(),
    };
    for part in parts {
        if !matches!(part, Component::RootDir | Component::CurDir) {
            key.push(part.as_os_str());
        }
    }
    key
}

pub(super) struct Tab {
    pub run: Option<String>,
    explicit_id: Option<String>,
    tui_ready_event: bool,
    agy_events: Vec<(String, u64, shell::AgyEvent)>,
    codex_display: Option<(String, Option<String>)>,
    last_codex_display: Option<(String, Option<String>)>,
    program_status: &'static str,
    grok_title_status: &'static str,
    pub meter: Option<&'static str>,
    screen_state: Option<&'static str>,
    pub bound: Option<crate::transcript::Key>,
    pub expected: Option<crate::transcript::Key>,
    restore_end: Option<shell::RestoreEnd>,
    pub phase: String,
    pub preview: String,
    pub prompt_count: usize,
    pub last_activity_ms: Option<i64>,
    pub unread: bool,
    was_running: bool,
    was_waiting: bool,
    pub id: SessionId,
    pub directory: PathBuf,
    pub project: PathBuf,
    pub title: String,
    host_title: bool,
    pub foreground: Option<shell::Foreground>,
    pub opening: Option<Startup>,
    pub session: Option<Session>,
    pub proxy: Option<Arc<SessionProxy>>,
    pub proxy_summary: String,
    pub auto_command: Option<String>,
    pub closing: bool,
    pub ended: Option<End>,
    pub error: Option<String>,
    resolving: Option<mpsc::Receiver<(PathBuf, PathBuf)>>,
    discovered: Option<PathBuf>,
}

impl Tab {
    pub(super) fn pending(id: SessionId, directory: PathBuf, opening: Startup) -> Self {
        Self {
            id,
            run: None,
            explicit_id: None,
            tui_ready_event: false,
            agy_events: Vec::new(),
            codex_display: None,
            last_codex_display: None,
            program_status: "",
            grok_title_status: "",
            meter: None,
            screen_state: None,
            bound: None,
            expected: None,
            restore_end: None,
            phase: String::new(),
            preview: String::new(),
            prompt_count: 0,
            last_activity_ms: None,
            unread: false,
            was_running: false,
            was_waiting: false,
            project: project_key(&directory),
            directory,
            opening: Some(opening),
            title: String::new(),
            host_title: false,
            foreground: None,
            session: None,
            proxy: None,
            proxy_summary: String::new(),
            auto_command: None,
            closing: false,
            ended: None,
            error: None,
            resolving: None,
            discovered: None,
        }
    }
    fn resolve_directory(&mut self, wake: &Waker) {
        // File I/O stays off the renderer and the ConPTY reader. Coalesce rapid cd events.
        if self.resolving.is_some()
            || !matches!(self.directory.components().next(),
            Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            return;
        }
        let path = self.directory.clone();
        let wake = wake.clone();
        let (send, recv) = mpsc::channel();
        self.resolving = Some(recv);
        std::thread::spawn(move || {
            let resolved = path.canonicalize().unwrap_or_else(|_| path.clone());
            let _ = send.send((path, project_key(&resolved)));
            wake();
        });
    }
    fn update_project(&mut self, wake: &Waker) -> bool {
        let Some(resolver) = &self.resolving else {
            return false;
        };
        let (path, key) = match resolver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.resolving = None;
                return false;
            }
        };
        self.resolving = None;
        if path != self.directory {
            self.resolve_directory(wake);
            return false;
        }
        let changed = self.project != key;
        self.project = key;
        changed
    }
    fn poll(&mut self, active: bool, size: Size, wake: &Waker) -> (bool, bool) {
        let mut changed = self.update_project(wake);
        let mut opened = false;
        if let Some(result) = self
            .opening
            .as_ref()
            .and_then(|recv| match recv.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err("启动线程已结束".into())),
            })
        {
            self.opening = None;
            changed = true;
            match result {
                Ok(s) => {
                    debug_assert_eq!(self.id, s.id);
                    s.listener.visible.store(active, Ordering::Release);
                    if self.closing {
                        s.request_close();
                    } else if active && let Err(e) = s.resize(size) {
                        self.error = Some(e.to_string());
                    }
                    self.session = Some(s);
                    opened = true;
                    if let Some(cmd) = self.auto_command.take()
                        && let Some(session) = &self.session
                    {
                        session.input(format!("{cmd}\r").into_bytes());
                    }
                    self.resolve_directory(wake);
                }
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(s) = &self.session
            && (s.listener.sidebar_dirty.swap(false, Ordering::AcqRel) || opened)
        {
            let updates = std::mem::take(&mut *s.listener.shell.lock().unwrap());
            self.restore_end = updates.restore_end;
            let end = s.end.lock().unwrap().clone();
            if end != self.ended {
                self.ended = end;
                changed = true;
            }
            if let Some(agent) = updates.foreground {
                if agent != self.foreground || updates.launch != self.run {
                    self.screen_state = None;
                    self.title.clear();
                    self.host_title = false;
                    self.codex_display = None;
                    self.last_codex_display = None;
                    self.tui_ready_event = false;
                }
                self.program_status = "";
                self.grok_title_status = "";
                self.meter = None;
                changed |= agent != self.foreground || updates.launch != self.run;
                self.run = updates.launch;
                self.explicit_id = updates.explicit_id;
                self.foreground = agent;
            }
            if updates.tui_ready {
                self.tui_ready_event = true;
            }
            if let Some(state) = updates.screen_state {
                changed |= self.screen_state != state;
                self.screen_state = state;
            }
            if let Some((agent, value)) = updates.status
                && self.foreground == Some(agent)
            {
                if !(value.is_empty() && self.program_status == "出错") {
                    changed |= self.program_status != value;
                    self.program_status = value;
                }
            }
            self.agy_events = updates.agy_events;
            if self.ended.is_some() {
                self.foreground = None;
                self.run = None;
                self.explicit_id = None;
            }
            if let Some(title) = updates.title {
                changed |= self.update_title(&title);
            }
            if let Some(path) = updates.directory
                && path != self.directory
            {
                self.project = project_key(&path);
                self.directory = path;
                self.resolve_directory(wake);
                changed = true;
            }
            if let Some(summary) = updates.proxy_summary
                && summary != self.proxy_summary
            {
                self.proxy_summary = summary;
                changed = true;
            }
        }
        (changed, opened)
    }
    pub fn cancel_restore(&mut self) {
        if self.expected.take().is_some()
            && let Some(session) = &self.session
        {
            session.claim_restore();
        }
    }
    pub fn state_text(&self) -> String {
        if self.closing {
            return "关闭中".into();
        }
        if self.error.is_some() {
            return "出错".into();
        }
        if self.opening.is_some() {
            return "启动中".into();
        }
        if self.foreground.is_some() && self.ended.is_none() {
            if let Some(state) = self.screen_state {
                return state.into();
            }
            if matches!(self.phase.as_str(), "出错" | "已打断")
                && !matches!(
                    self.meter.unwrap_or(self.program_status),
                    "运行中" | "等待用户"
                )
            {
                return self.phase.clone();
            }
            if let Some(value) = self.meter {
                return value.into();
            }
            if !self.program_status.is_empty() {
                return self.program_status.into();
            }
            if !self.grok_title_status.is_empty() {
                return self.grok_title_status.into();
            }
            return match self.phase.as_str() {
                "" | "状态未知" => "在线".into(),
                phase if phase.starts_with("待同步") => "在线".into(),
                phase => phase.into(),
            };
        }
        match &self.ended {
            Some(End::Code(code)) => format!("已退出 · {code}"),
            Some(End::Error(_)) => "异常退出".into(),
            Some(End::Closed) => "已关闭".into(),
            None => "在线".into(),
        }
    }

    pub(crate) fn update_title(&mut self, raw: &str) -> bool {
        if self.foreground == Some(shell::Foreground::Codex)
            && let Some(display) = shell::codex_display(raw)
            && self.last_codex_display.as_ref() != Some(&display)
        {
            self.last_codex_display = Some(display.clone());
            self.codex_display = Some(display);
        }
        let mut status_changed = false;
        if self.foreground == Some(shell::Foreground::Grok)
            && let Some(state) = shell::grok_title_state(raw)
        {
            status_changed = self.grok_title_status != state;
            self.grok_title_status = state;
        }
        // ConPTY can report the host executable while the agent owns the terminal.
        // Inspect the complete title before applying the UI's 96-character limit.
        let title = raw.trim();
        let path = Path::new(title.trim_matches('"'));
        let shell_exe = path.file_name().is_some_and(|file| {
            (path.is_absolute() || file == path.as_os_str())
                && (file.eq_ignore_ascii_case("pwsh.exe")
                    || file.eq_ignore_ascii_case("powershell.exe"))
        });
        let shell_version = title.split_once(' ').is_some_and(|(name, version)| {
            name.eq_ignore_ascii_case("PowerShell")
                && version.as_bytes().first().is_some_and(u8::is_ascii_digit)
                && version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        });
        let host_title = shell_exe
            || shell_version
            || title.eq_ignore_ascii_case("PowerShell")
            || title.eq_ignore_ascii_case("Windows PowerShell");
        let title: String = raw.chars().filter(|c| !c.is_control()).take(96).collect();
        let changed = title != self.title || host_title != self.host_title || status_changed;
        self.title = title;
        self.host_title = host_title;
        changed
    }

    fn display_title(&self) -> &str {
        if self.host_title {
            ""
        } else {
            self.title.trim()
        }
    }

    pub fn label(&self) -> String {
        match self.foreground {
            Some(shell::Foreground::Codex) => return "Codex".into(),
            Some(shell::Foreground::Agy) => return "agy".into(),
            Some(shell::Foreground::Kimi) => return "Kimi".into(),
            Some(shell::Foreground::Grok) => return "Grok".into(),
            None => (),
        }
        let title = self.display_title();
        if title.is_empty() {
            "PowerShell".into()
        } else {
            format!("终端 · {title}")
        }
    }

    pub fn reset_reply(&mut self) {
        self.was_running = false;
        self.was_waiting = false;
        self.unread = false;
        self.last_activity_ms = None;
    }

    pub(super) fn caption(&self, spinner: usize) -> String {
        let project = self
            .directory
            .file_name()
            .unwrap_or(self.directory.as_os_str())
            .to_string_lossy();
        let label = self.label();
        let mut title = String::new();
        if let Some(agent) = self.foreground {
            if self.state_text() == "运行中" {
                title.push_str(crate::sidebar::SPINNER[spinner % crate::sidebar::SPINNER.len()]);
                title.push(' ');
            }
            let raw = self.display_title();
            let name = match agent {
                shell::Foreground::Codex => shell::codex_title_parts(raw)
                    .map_or(raw, |(_, rest)| rest.split(" | ").next().unwrap_or("")),
                shell::Foreground::Grok => shell::grok_title_name(raw),
                _ => raw,
            };
            if !name.is_empty()
                && !crate::hook_wire::valid_id(name)
                && !name.eq_ignore_ascii_case(&label)
                && !(agent == shell::Foreground::Agy
                    && name.eq_ignore_ascii_case("Antigravity CLI"))
                && !(agent == shell::Foreground::Kimi && name.eq_ignore_ascii_case("Kimi Code"))
                && name != project.as_ref()
            {
                title.extend(name.chars().take(54));
                title.push_str(" · ");
            }
        }
        if !project.is_empty() {
            title.push_str(&project);
            title.push_str(" · ");
        }
        title.push_str(&label);
        title.push_str(" · ");
        title.push_str(crate::single_instance::app_display_name());
        title
    }

    pub fn observe_reply(&mut self, selected: bool) -> Option<crate::sound::ReplyNotification> {
        if self.foreground.is_none() || self.ended.is_some() || self.closing {
            self.reset_reply();
            return None;
        }
        let state = self.state_text();
        if state == "运行中" {
            self.unread = false;
        }
        if self.foreground == Some(shell::Foreground::Codex)
            && self.expected.as_ref().is_some_and(|k| k.agent == "codex")
        {
            self.was_running = false;
            self.was_waiting = state == "等待用户";
            return None;
        }
        let mut notified = None;
        if self.was_running && state == "就绪" {
            self.mark_reply(selected);
            notified = Some(crate::sound::ReplyNotification::Completed);
        } else if !self.was_waiting && state == "等待用户" {
            notified = Some(crate::sound::ReplyNotification::Waiting);
        }
        self.was_running = state == "运行中";
        self.was_waiting = state == "等待用户";
        notified
    }
    pub fn mark_reply(&mut self, selected: bool) {
        self.unread = !selected;
        self.last_activity_ms = Some((crate::hook_wire::timestamp_micros() / 1000) as i64);
    }
}

pub(super) struct Workspace {
    pub caption_brand: std::cell::OnceCell<rune::Image>,
    pub layout: crate::workbench::Layout,
    pub desktop: crate::desktop_info::DesktopInfo,
    pub spinner: usize,
    next_spinner: Option<f64>,
    window_title: String,
    window_file: Option<crate::workspace_file::WindowFile>,
    saved_window: Option<crate::workspace_file::Snapshot>,
    deferred: VecDeque<ShellLaunch>,
    pub history: crate::history_view::Panel,
    pub clawbot: crate::clawbot::bridge::ClawbotPanel,
    pub tabs: Vec<Tab>,
    pub active: Option<SessionId>,
    pub wake: Waker,
    pub size: Size,
    pub cell: f32,
    pub scale: f32,
    pub screen: Screen,
    pub focused: bool,
    pub window_active: bool,
    pub preedit: String,
    pub composing: bool,
    pub discard_commit: bool,
    pub focus_generation: u64,
    pub focus_handoff: Option<SessionId>,
    pub force: bool,
    pub mouse_down: bool,
    pub last_cell: Option<(u16, u16)>,
    pub last_drag: Option<(u16, u16, bool)>,
    pub wheel_remainder: f32,
    pub first_open: bool,
    #[cfg(all(windows, not(test)))]
    app_handle: Option<rune::AppHandle<Workspace>>,
    #[cfg(all(windows, not(test)))]
    tray: Option<crate::tray::Tray>,
}

impl Workspace {
    pub fn open() -> Self {
        Self {
            caption_brand: Default::default(),
            layout: crate::workbench::Layout::new(rune::prefs::Prefs::memory()),
            desktop: Default::default(),
            spinner: 0,
            next_spinner: None,
            window_title: String::new(),
            window_file: None,
            saved_window: None,
            deferred: VecDeque::new(),
            history: Default::default(),
            clawbot: Default::default(),
            tabs: Vec::new(),
            active: None,
            first_open: true,
            wake: std::sync::Arc::new(|| {}),
            size: Size::default(),
            cell: 9.,
            scale: 1.,
            screen: Screen::default(),
            focused: false,
            window_active: true,
            preedit: String::new(),
            composing: false,
            discard_commit: false,
            focus_generation: 0,
            focus_handoff: None,
            force: true,
            mouse_down: false,
            last_cell: None,
            last_drag: None,
            wheel_remainder: 0.,
            #[cfg(all(windows, not(test)))]
            app_handle: None,
            #[cfg(all(windows, not(test)))]
            tray: None,
        }
    }

    #[cfg(all(windows, not(test)))]
    pub fn set_app_handle(&mut self, handle: rune::AppHandle<Workspace>) {
        self.app_handle = Some(handle);
    }

    pub fn sync_tray(&mut self) {
        #[cfg(all(windows, not(test)))]
        {
            if self.layout.close_to_tray {
                if self.tray.is_none()
                    && let Some(handle) = self.app_handle.clone()
                {
                    match crate::tray::Tray::start(handle) {
                        Ok(tray) => self.tray = Some(tray),
                        Err(e) => self.history.error = e,
                    }
                }
            } else {
                self.tray.take();
            }
        }
    }

    pub fn set_tray_tooltip(&self, tip: &str) {
        #[cfg(all(windows, not(test)))]
        if let Some(tray) = &self.tray {
            tray.set_tooltip(tip);
        }
        #[cfg(not(all(windows, not(test))))]
        let _ = tip;
    }
    pub fn notify_agent_sound(&self, kind: crate::sound::ReplyNotification, selected: bool) {
        if self
            .layout
            .sound
            .should_play(kind, self.window_active, selected)
        {
            crate::sound::play_debounced(self.layout.sound.preset);
        }
    }
    pub fn active_index(&self) -> Option<usize> {
        let id = self.active?;
        self.tabs.iter().position(|tab| tab.id == id)
    }
    pub fn current(&self) -> Option<&Tab> {
        self.active_index().map(|i| &self.tabs[i])
    }
    pub fn session(&self) -> Option<&Session> {
        self.current()?.session.as_ref()
    }
    pub fn writable(&self) -> bool {
        self.current()
            .is_some_and(|tab| tab.session.is_some() && tab.ended.is_none() && !tab.closing)
    }
    pub fn invalidate_input(&mut self) {
        self.focus_generation += 1;
        self.focus_handoff = None;
        self.discard_commit |= self.composing;
        self.composing = false;
        self.preedit.clear();
        self.mouse_down = false;
        self.last_cell = None;
        self.last_drag = None;
        self.wheel_remainder = 0.;
    }
    pub fn refresh_active_screen(&mut self, force: bool) -> bool {
        let Some(i) = self.active_index() else {
            return false;
        };
        let Some(s) = &self.tabs[i].session else {
            return false;
        };
        let force = force || self.force;
        s.listener.dirty.swap(false, Ordering::AcqRel);
        let changed = self.screen.refresh(s, force, self.cell, self.scale);
        self.force = false;
        changed
    }
    pub fn select(&mut self, id: Option<SessionId>, tx: &mut Tx) {
        tx.instant();
        if let Some(index) = self
            .deferred
            .iter()
            .position(|launch| Some(launch.id) == id)
        {
            self.deferred
                .remove(index)
                .unwrap()
                .start(self.size, self.wake.clone());
        }
        if let Some(tab) = self.tabs.iter_mut().find(|t| Some(t.id) == id) {
            tab.unread = false;
        }
        if self.active != id {
            if let Some(tab) = self.tabs.iter().find(|t| Some(t.id) == id) {
                self.layout.collapsed.remove(&tab.project);
            }
            if let Some(s) = self.session() {
                if self.focused {
                    s.focus_event(false);
                }
                s.listener.visible.store(false, Ordering::Release);
            }
            self.invalidate_input();
            self.focused = false;
            self.active = id;
            self.reset_drawer();
            self.force = true;
            if let Some(i) = self.active_index()
                && let Some(s) = &self.tabs[i].session
            {
                s.listener.visible.store(true, Ordering::Release);
                if self.window_active {
                    // The new child must receive focus before a key queued behind this click.
                    s.focus_event(true);
                    self.focused = true;
                }
                let failure = s.resize(self.size).err();
                s.listener.dirty.swap(false, Ordering::AcqRel);
                self.screen.refresh(s, true, self.cell, self.scale);
                self.force = false;
                if let Some(e) = failure {
                    self.tabs[i].error = Some(e.to_string());
                }
            } else {
                self.screen = Screen::default();
            }
        }
        if self.session().is_some() {
            // A pointer release and a keystroke can precede the next Rune focus commit.
            self.focus_handoff = self.active;
            tx.focus("terminal", false);
        }
    }
    pub fn proxy_catalog_path(&self) -> Option<PathBuf> {
        self.layout.proxy_catalog_path().or_else(|| {
            self.window_file
                .as_ref()
                .and_then(|f| f.folder().parent())
                .map(crate::proxy::catalog_path)
        })
    }
    pub fn new_shell(&mut self, directory: Option<PathBuf>, tx: &mut Tx) {
        self.open_shell(directory, None, tx);
    }
    pub fn open_shell(
        &mut self,
        directory: Option<PathBuf>,
        resume: Option<crate::transcript::Key>,
        tx: &mut Tx,
    ) {
        let launch = self.prepare_shell(directory, resume, None, None, None);
        self.select(Some(launch.id), tx);
        launch.start(self.size, self.wake.clone());
    }
    pub fn open_shell_with_proxy(
        &mut self,
        directory: Option<PathBuf>,
        resume: Option<crate::transcript::Key>,
        proxy_seed: Option<String>,
        initial_summary: Option<String>,
        auto_command: Option<String>,
        tx: &mut Tx,
    ) -> SessionId {
        let launch =
            self.prepare_shell(directory, resume, proxy_seed, initial_summary, auto_command);
        let id = launch.id;
        self.select(Some(id), tx);
        launch.start(self.size, self.wake.clone());
        id
    }
    pub fn reopen_tab_with_proxy(
        &mut self,
        id: SessionId,
        target: &crate::proxy::ProxyTarget,
        tx: &mut Tx,
    ) -> Result<(SessionId, bool), String> {
        let Some(tab) = self.tabs.iter().find(|t| t.id == id && !t.closing) else {
            return Err("目标终端已关闭".into());
        };
        let dir = tab.directory.clone();
        if !dir.is_dir() {
            return Err(format!("项目目录已不存在：{}", dir.display()));
        }
        let resume = tab.bound.as_ref().or(tab.expected.as_ref()).cloned();
        let auto_command = if resume.is_some() {
            None
        } else {
            tab.foreground.map(|f| f.name().to_owned())
        };
        let has_resume = resume.is_some();
        let seal =
            crate::proxy::seal_target(target).map_err(|e| format!("加密代理配置失败：{e}"))?;
        let new_id = self.open_shell_with_proxy(
            Some(dir),
            resume,
            Some(seal),
            Some(target.initial_summary()),
            auto_command,
            tx,
        );
        self.close(id, tx);
        self.select(Some(new_id), tx);
        Ok((new_id, has_resume))
    }
    fn prepare_shell(
        &mut self,
        directory: Option<PathBuf>,
        resume: Option<crate::transcript::Key>,
        mut saved_proxy: Option<String>,
        mut initial_summary: Option<String>,
        auto_command: Option<String>,
    ) -> ShellLaunch {
        let cwd = directory.map(Ok).unwrap_or_else(shell::starting_directory);
        let id = SessionId::new();
        let (send, recv) = mpsc::channel();
        if saved_proxy.is_none()
            && resume.is_none()
            && let Some(default_target) = self.layout.proxy.default_target()
        {
            if let Ok(seal) = crate::proxy::seal_target(&default_target) {
                saved_proxy = Some(seal);
            }
            if initial_summary.is_none() {
                initial_summary = Some(default_target.initial_summary());
            }
        }
        let mut tab = Tab::pending(id, cwd.as_ref().ok().cloned().unwrap_or_default(), recv);
        tab.expected = resume.clone();
        if let Some(summary) = initial_summary {
            tab.proxy_summary = summary;
        }
        tab.auto_command = auto_command;
        let proxy = self
            .window_file
            .as_ref()
            .map(|f| Arc::new(SessionProxy::new(f.folder(), id, saved_proxy)));
        tab.proxy = proxy.clone();
        let proxy_catalog = self.proxy_catalog_path();
        self.tabs.push(tab);
        self.first_open = false;
        ShellLaunch {
            id,
            cwd,
            resume,
            proxy,
            proxy_catalog,
            send,
        }
    }
    #[cfg(test)]
    pub fn add_pending(&mut self, id: SessionId, cwd: PathBuf, recv: Startup, tx: &mut Tx) {
        self.first_open = false;
        self.tabs.push(Tab::pending(id, cwd, recv));
        self.select(Some(id), tx);
    }
    pub fn close(&mut self, id: SessionId, tx: &mut Tx) {
        tx.instant();
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        self.tabs[index].closing = true;
        self.deferred.retain(|launch| launch.id != id);
        if let Some(s) = &self.tabs[index].session {
            s.request_close();
        }
        if self.active == Some(id) {
            let next = self.tabs[index + 1..]
                .iter()
                .chain(self.tabs[..index].iter().rev())
                .find(|tab| !tab.closing)
                .map(|tab| tab.id);
            self.select(next, tx);
        } else if self.session().is_some() {
            // The close button just took focus, including when its tab was in the background.
            self.invalidate_input();
            tx.focus_when_shown("terminal", false);
        }
    }
    pub fn poll(&mut self, clock: f64, tx: &mut Tx) -> bool {
        let mut changed = self.poll_split_size(clock, tx);
        changed |= self.poll_width_error();
        if self.first_open {
            if let Some(snapshot) = self.saved_window.take() {
                self.first_open = false;
                for tab in snapshot.tabs {
                    let launch = self.prepare_shell(
                        Some(tab.directory),
                        tab.conversation,
                        tab.proxy,
                        None,
                        None,
                    );
                    self.deferred.push_back(launch);
                }
                let selected = self
                    .tabs
                    .get(snapshot.selected.unwrap_or(0))
                    .or(self.tabs.first())
                    .map(|tab| tab.id);
                self.select(selected, tx);
                self.layout.collapsed = snapshot.collapsed;
            } else {
                self.new_shell(None, tx);
            }
            changed = true;
        }
        if !self.deferred.is_empty()
            && self.current().is_none_or(|t| t.opening.is_none())
            && let Some(launch) = self.deferred.pop_front()
        {
            launch.start(self.size, self.wake.clone());
        }
        for tab in &mut self.tabs {
            let selected = self.active == Some(tab.id);
            let previous = tab.run.clone();
            let (updated, opened) = tab.poll(selected, self.size, &self.wake);
            if tab.discovered.as_ref() != Some(&tab.project)
                && tab.project.is_absolute()
                && let Some(service) = &self.history.service
            {
                let _ = service
                    .send
                    .send(crate::history::Command::Discover(tab.project.clone()));
                tab.discovered = Some(tab.project.clone());
            }
            changed |= updated;
            if let Some(end) = tab.restore_end.take()
                && tab.expected.take().is_some()
            {
                match end {
                    shell::RestoreEnd::Exhausted => {
                        self.history.error =
                            "会话恢复失败，已重试 3 次。可从历史会话再次恢复。".into()
                    }
                    shell::RestoreEnd::Unavailable => {
                        self.history.error = "无法启动恢复，请查看该终端中的提示。".into()
                    }
                    shell::RestoreEnd::Finished => (),
                }
                changed = true;
            }
            if previous != tab.run || (tab.closing && tab.run.is_some()) {
                if tab.closing {
                    tab.run = None;
                }
                tab.bound = None;
                tab.reset_reply();
                tab.preview.clear();
                tab.phase.clear();
                tab.prompt_count = 0;
                if let Some(service) = &self.history.service {
                    let cmd = match (&tab.run, tab.foreground) {
                        (Some(run), Some(agent)) => crate::history::Command::Foreground {
                            marker: tab.id.marker(),
                            run: run.clone(),
                            agent: agent.name().into(),
                            project: tab.project.clone(),
                            restore: tab.expected.clone(),
                            initial: tab
                                .explicit_id
                                .as_deref()
                                .and_then(|id| crate::transcript::Key::new(agent.name(), id)),
                            proxy: tab.proxy.clone(),
                        },
                        _ => crate::history::Command::Leave(tab.id.marker()),
                    };
                    let _ = service.send.send(cmd);
                }
            }
            let had_agy_events = !tab.agy_events.is_empty();
            for (run, sent, event) in tab.agy_events.drain(..) {
                if tab.run.as_ref() != Some(&run) || tab.foreground != Some(shell::Foreground::Agy)
                {
                    continue;
                }
                let fallback = match &event {
                    shell::AgyEvent::Status(value) | shell::AgyEvent::ProgramStatus(value) => {
                        Some(*value)
                    }
                    _ => None,
                };
                let cmd = crate::history::Command::Agy(tab.id.marker(), run, sent, event);
                if !self
                    .history
                    .service
                    .as_ref()
                    .is_some_and(|s| s.send.send(cmd).is_ok())
                    && let Some(value) = fallback
                    && !(value.is_empty() && tab.program_status == "出错")
                {
                    changed |= tab.meter.take().is_some() || tab.program_status != value;
                    tab.program_status = value;
                }
            }
            if std::mem::take(&mut tab.tui_ready_event)
                && let Some(run) = &tab.run
                && !tab.closing
                && let Some(service) = &self.history.service
            {
                let _ = service.send.send(crate::history::Command::TuiReady(
                    tab.id.marker(),
                    run.clone(),
                ));
            }
            if let Some(display) = tab.codex_display.take()
                && let Some(run) = &tab.run
                && !tab.closing
                && tab.foreground == Some(shell::Foreground::Codex)
                && let Some(service) = &self.history.service
            {
                let _ = service.send.send(crate::history::Command::CodexTitle(
                    tab.id.marker(),
                    run.clone(),
                    display,
                ));
            }
            if selected && opened {
                self.force = true;
                if !crate::workbench::drawer_focused(tx)
                    && !tx
                        .focused()
                        .is_some_and(|k| k.split('/').any(|p| p.starts_with("history-")))
                {
                    tx.focus_when_shown("terminal", false);
                }
            }
            if updated || previous != tab.run || had_agy_events {
                if let Some(kind) = tab.observe_reply(selected)
                    && self
                        .layout
                        .sound
                        .should_play(kind, self.window_active, selected)
                {
                    crate::sound::play_debounced(self.layout.sound.preset);
                }
            }
        }
        let old_len = self.tabs.len();
        self.tabs.retain(|tab| {
            !(tab.closing
                && tab.opening.is_none()
                && (tab.ended.is_some() || tab.session.is_none()))
        });
        changed |= self.tabs.len() != old_len;
        if changed {
            self.desktop.projects(&self.tabs);
        }
        let focus = self.session().is_some() && self.terminal_has_focus(tx);
        if tx.focused().and_then(|key| key.rsplit('/').next()) == Some("terminal") {
            self.focus_handoff = None;
        }
        if self.focused != focus {
            if !focus {
                self.invalidate_input();
            }
            self.focused = focus;
            if let Some(s) = self.session() {
                s.focus_event(focus);
            }
            changed = true;
        }
        if let Some(i) = self.active_index()
            && let Some(s) = &self.tabs[i].session
            && (s.listener.dirty.swap(false, Ordering::AcqRel) || self.force)
        {
            changed |= self.screen.refresh(s, self.force, self.cell, self.scale);
            self.force = false;
        }
        changed |= self.poll_history(tx);
        changed |= self.poll_clawbot(clock, tx);
        changed |= self.update_caption(clock, tx);
        self.desktop.poll() || changed
    }
    fn update_caption(&mut self, clock: f64, tx: &mut Tx) -> bool {
        let query = self.layout.search.text.trim().to_lowercase();
        let running = !self.layout.is_dragging()
            && self.tabs.iter().any(|t| {
                t.foreground.is_some()
                    && t.state_text() == "运行中"
                    && (self.active == Some(t.id)
                        || (!self.layout.settings
                            && !self
                                .layout
                                .collapsed_for_search(!query.is_empty())
                                .contains(&t.project)
                            && self.layout.filter.includes(t, false)
                            && self.layout.matches_search(t, &query)))
            });
        let mut changed = false;
        if running {
            if self.next_spinner.is_none_or(|next| clock >= next) {
                self.spinner = (self.spinner + 1) % crate::sidebar::SPINNER.len();
                self.next_spinner = Some(clock + 0.125);
                changed = true;
            }
            tx.wake_after(std::time::Duration::from_secs_f64(
                (self.next_spinner.unwrap() - clock).max(0.001),
            ));
        } else {
            self.next_spinner = None;
            self.spinner = 0;
        }
        let title = self.current().map_or_else(
            || crate::single_instance::app_display_name().into(),
            |tab| tab.caption(self.spinner),
        );
        if title != self.window_title {
            tx.set_title(&title);
            self.window_title = title;
        }
        changed
    }
    pub fn groups(&self) -> Vec<Vec<&Tab>> {
        let mut groups: Vec<Vec<&Tab>> = Vec::new();
        for tab in &self.tabs {
            if let Some(group) = groups.iter_mut().find(|g| g[0].project == tab.project) {
                group.push(tab);
            } else {
                groups.push(vec![tab]);
            }
        }
        groups
    }
    pub fn enable_restore(&mut self, folder: &Path) {
        match crate::workspace_file::WindowFile::claim(folder) {
            Ok((file, snapshot, warning)) => {
                self.window_file = Some(file);
                self.saved_window = snapshot;
                if let Some(warning) = warning {
                    self.history.error = warning;
                }
            }
            Err(e) => self.history.error = format!("工作台恢复不可用 · {e}"),
        }
    }
    pub fn save_window(&mut self, _: &mut Tx) {
        self.layout.finish_width_write();
        if !self.first_open
            && let Some(file) = &self.window_file
            && let Err(e) =
                crate::workspace_file::Snapshot::from_window(self).and_then(|s| file.save(&s))
        {
            self.history.error = format!("未能保存工作台 · {e}");
            eprintln!("{}", self.history.error);
        }
    }
    pub fn on_close_window(&mut self, tx: &mut Tx) {
        self.save_window(tx);
        #[cfg(all(windows, not(test)))]
        let keep = self.layout.close_to_tray && self.tray.is_some();
        #[cfg(not(all(windows, not(test))))]
        let keep = self.layout.close_to_tray;
        if !keep {
            tx.quit();
        }
    }
    pub fn on_quit_app(&mut self, tx: &mut Tx) {
        self.clawbot.stop();
        self.save_window(tx);
        #[cfg(all(windows, not(test)))]
        self.tray.take();
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.clawbot.stop();
        #[cfg(all(windows, not(test)))]
        self.tray.take();
        self.deferred.clear();
        // Signal all live shells first so their shutdown grace periods overlap.
        for tab in &self.tabs {
            if let Some(s) = &tab.session {
                s.request_close();
            }
        }
        for tab in &mut self.tabs {
            if let Some(recv) = tab.opening.take()
                && let Ok(Ok(s)) = recv.recv()
            {
                s.request_close();
                tab.session = Some(s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_host_titles_do_not_become_agent_conversation_names() {
        let mut tab = Tab::pending(SessionId::new(), r"L:\AI\temp".into(), mpsc::channel().1);
        tab.opening = None;
        let long = format!(r"C:\{}\PowerShell\7\PWSH.EXE", "long-folder\\".repeat(12));
        for (agent, label) in [
            (shell::Foreground::Agy, "agy"),
            (shell::Foreground::Codex, "Codex"),
        ] {
            tab.foreground = Some(agent);
            for host in [
                r"C:\Program Files\PowerShell\7\pwsh.exe",
                r#""C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe""#,
                "PWSH.EXE",
                "Windows PowerShell",
                "PowerShell 7.5.4",
                &long,
            ] {
                tab.update_title("上一段对话");
                assert!(tab.update_title(host), "{host}");
                assert_eq!(tab.title, host.chars().take(96).collect::<String>());
                assert_eq!(tab.caption(0), format!("temp · {label} · Mcli"));
                assert!(!tab.update_title(host));
            }
            for topic in ["PowerShell 7 代理排查", r"检查 C:\tools\pwsh.exe"] {
                tab.update_title(topic);
                assert_eq!(tab.caption(0), format!("{topic} · temp · {label} · Mcli"));
            }
        }
        tab.foreground = None;
        tab.update_title(r"C:\Program Files\PowerShell\7\pwsh.exe");
        assert_eq!(tab.caption(0), "temp · PowerShell · Mcli");
        tab.update_title("本地构建");
        assert_eq!(tab.label(), "终端 · 本地构建");
    }

    #[test]
    fn selected_title_follows_its_agent_and_hidden_work_does_not_keep_animating() {
        let mut rt = crate::app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
        let ids = [SessionId::new(), SessionId::new()];
        rt.update(|w, tx| {
            w.first_open = false;
            for (id, agent, dir, title) in [
                (ids[0], shell::Foreground::Codex, r"C:\project-A", "第一段"),
                (ids[1], shell::Foreground::Agy, r"C:\project-B", "第二段"),
            ] {
                let mut tab = Tab::pending(id, dir.into(), mpsc::channel().1);
                tab.opening = None;
                tab.foreground = Some(agent);
                tab.phase = "运行中".into();
                tab.title = title.into();
                w.tabs.push(tab);
            }
            w.select(Some(ids[0]), tx);
        });
        rt.frame(0.);
        let first = rt.state.window_title.clone();
        assert!(first.ends_with("第一段 · project-A · Codex · Mcli"));
        rt.frame(0.130);
        assert_ne!(rt.state.window_title, first);
        rt.update(|w, tx| w.select(Some(ids[1]), tx));
        rt.frame(0.14);
        assert!(
            rt.state
                .window_title
                .ends_with("第二段 · project-B · agy · Mcli")
        );
        rt.update(|w, _| w.tabs[1].phase = "等待用户".into());
        rt.frame(0.26);
        assert_eq!(rt.state.window_title, "第二段 · project-B · agy · Mcli");
        assert!(rt.state.next_spinner.is_some());
        rt.update(|w, _| {
            w.layout.collapsed.insert(w.tabs[0].project.clone());
        });
        rt.frame(0.39);
        assert!(rt.state.next_spinner.is_none());
        assert_eq!(rt.state.window_title, "第二段 · project-B · agy · Mcli");
    }
}
