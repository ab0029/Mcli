use crate::terminal::Size;
use crate::workspace::{Tab, Workspace};
use rune::prefs::Prefs;
use rune::{Cursor, Drag, El, Key, KeyEvent, Phase, Tx, el, row};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf, sync::mpsc, thread};

const DEFAULT_WIDTH: f32 = 248.;
const MIN_WIDTH: f32 = 180.;
const MAX_WIDTH: f32 = 420.;
pub const GRIP: f32 = 8.;
pub const DRAWER: f32 = 280.;
const REFLOW_PAUSE: std::time::Duration = std::time::Duration::from_millis(180);
const WINDOW_RESIZE_COOLDOWN: std::time::Duration = std::time::Duration::from_millis(90);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    Search,
    Footprints,
    Archive,
    NewShell,
    PrevTab,
    NextTab,
}

impl ShortcutAction {
    pub const ALL: [Self; 6] = [
        Self::Search,
        Self::Footprints,
        Self::Archive,
        Self::NewShell,
        Self::PrevTab,
        Self::NextTab,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Footprints => "footprints",
            Self::Archive => "archive",
            Self::NewShell => "new-shell",
            Self::PrevTab => "prev-tab",
            Self::NextTab => "next-tab",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Search => "搜索项目与会话",
            Self::Footprints => "提示词足迹",
            Self::Archive => "全部历史",
            Self::NewShell => "新建终端",
            Self::PrevTab => "上一个终端",
            Self::NextTab => "下一个终端",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

impl Shortcut {
    pub fn new(ctrl: bool, shift: bool, alt: bool, key: &str) -> Self {
        Self {
            ctrl,
            shift,
            alt,
            key: key.to_owned(),
        }
    }

    fn normalize_key(key: &Key) -> Option<String> {
        match key {
            Key::Command(ch) if ch.is_ascii_graphic() => Some(ch.to_ascii_uppercase().to_string()),
            Key::Text(s) if s.len() == 1 => {
                let ch = s.chars().next()?;
                ch.is_ascii_graphic()
                    .then(|| ch.to_ascii_uppercase().to_string())
            }
            Key::PageUp => Some("PageUp".into()),
            Key::PageDown => Some("PageDown".into()),
            Key::Home => Some("Home".into()),
            Key::End => Some("End".into()),
            Key::Up => Some("Up".into()),
            Key::Down => Some("Down".into()),
            Key::Left => Some("Left".into()),
            Key::Right => Some("Right".into()),
            Key::Tab => Some("Tab".into()),
            Key::Enter => Some("Enter".into()),
            Key::Insert => Some("Insert".into()),
            Key::Delete => Some("Delete".into()),
            Key::Backspace => Some("Backspace".into()),
            Key::Function(n @ 1..=12) => Some(format!("F{n}")),
            _ => None,
        }
    }

    fn is_function_key(key: &str) -> bool {
        key.strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=12).contains(&n))
    }

    pub fn from_event(e: &KeyEvent) -> Option<Self> {
        if e.mods.logo {
            return None;
        }
        let key = Self::normalize_key(&e.key)?;
        if !e.mods.ctrl && !e.mods.alt && !Self::is_function_key(&key) {
            return None;
        }
        Some(Self {
            ctrl: e.mods.ctrl,
            shift: e.mods.shift,
            alt: e.mods.alt,
            key,
        })
    }

    pub fn is_reserved_terminal(&self) -> bool {
        self.ctrl && !self.shift && !self.alt && matches!(self.key.as_str(), "C" | "V")
    }

    pub fn matches(&self, e: &KeyEvent) -> bool {
        if e.mods.logo
            || e.mods.ctrl != self.ctrl
            || e.mods.shift != self.shift
            || e.mods.alt != self.alt
        {
            return false;
        }
        Self::normalize_key(&e.key).is_some_and(|k| k.eq_ignore_ascii_case(&self.key))
    }

    pub fn display(&self) -> String {
        let mut parts = Vec::with_capacity(4);
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(&self.key);
        parts.join("+")
    }

    pub fn compact_display(&self) -> String {
        let mut parts = Vec::with_capacity(4);
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("⇧");
        }
        parts.push(&self.key);
        parts.join(" ")
    }

    pub fn help_keys(&self) -> Vec<String> {
        let mut parts = Vec::with_capacity(4);
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        parts.push(self.key.clone());
        parts
    }
}

fn default_search_shortcut() -> Shortcut {
    Shortcut::new(true, true, false, "K")
}
fn default_footprints_shortcut() -> Shortcut {
    Shortcut::new(true, true, false, "H")
}
fn default_archive_shortcut() -> Shortcut {
    Shortcut::new(true, true, false, "L")
}
fn default_new_shell_shortcut() -> Shortcut {
    Shortcut::new(true, true, false, "T")
}
fn default_prev_tab_shortcut() -> Shortcut {
    Shortcut::new(true, false, false, "PageUp")
}
fn default_next_tab_shortcut() -> Shortcut {
    Shortcut::new(true, false, false, "PageDown")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcuts {
    #[serde(default = "default_search_shortcut")]
    pub search: Shortcut,
    #[serde(default = "default_footprints_shortcut")]
    pub footprints: Shortcut,
    #[serde(default = "default_archive_shortcut")]
    pub archive: Shortcut,
    #[serde(default = "default_new_shell_shortcut")]
    pub new_shell: Shortcut,
    #[serde(default = "default_prev_tab_shortcut")]
    pub prev_tab: Shortcut,
    #[serde(default = "default_next_tab_shortcut")]
    pub next_tab: Shortcut,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            search: default_search_shortcut(),
            footprints: default_footprints_shortcut(),
            archive: default_archive_shortcut(),
            new_shell: default_new_shell_shortcut(),
            prev_tab: default_prev_tab_shortcut(),
            next_tab: default_next_tab_shortcut(),
        }
    }
}

impl Shortcuts {
    pub fn get(&self, action: ShortcutAction) -> &Shortcut {
        match action {
            ShortcutAction::Search => &self.search,
            ShortcutAction::Footprints => &self.footprints,
            ShortcutAction::Archive => &self.archive,
            ShortcutAction::NewShell => &self.new_shell,
            ShortcutAction::PrevTab => &self.prev_tab,
            ShortcutAction::NextTab => &self.next_tab,
        }
    }

    pub fn set(&mut self, action: ShortcutAction, value: Shortcut) {
        match action {
            ShortcutAction::Search => self.search = value,
            ShortcutAction::Footprints => self.footprints = value,
            ShortcutAction::Archive => self.archive = value,
            ShortcutAction::NewShell => self.new_shell = value,
            ShortcutAction::PrevTab => self.prev_tab = value,
            ShortcutAction::NextTab => self.next_tab = value,
        }
    }

    pub fn conflict(&self, target: ShortcutAction, candidate: &Shortcut) -> Option<ShortcutAction> {
        ShortcutAction::ALL
            .into_iter()
            .find(|&action| action != target && self.get(action) == candidate)
    }

    pub fn match_event(&self, e: &KeyEvent) -> Option<ShortcutAction> {
        ShortcutAction::ALL
            .into_iter()
            .find(|&action| self.get(action).matches(e))
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

struct SplitDrag {
    previous_width: Option<f32>,
    pending_size: Option<Size>,
    next_reflow: f64,
}

enum PrefUpdate {
    SidebarWidth(Option<f32>),
    CloseToTray(bool),
    ShowMetricsBar(bool),
    Shortcuts(Shortcuts),
}

struct PrefsWriter {
    updates: Option<mpsc::Sender<PrefUpdate>>,
    errors: mpsc::Receiver<String>,
    worker: Option<thread::JoinHandle<()>>,
}

impl PrefsWriter {
    fn start(path: PathBuf, wake: rune::Waker) -> Self {
        let (updates, requests) = mpsc::channel::<PrefUpdate>();
        let (report, errors) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut prefs = Prefs::at(path);
            while let Ok(first) = requests.recv() {
                let mut width = None;
                let mut tray = None;
                let mut metrics = None;
                let mut shortcuts = None;
                for item in std::iter::once(first).chain(requests.try_iter()) {
                    match item {
                        PrefUpdate::SidebarWidth(v) => width = Some(v),
                        PrefUpdate::CloseToTray(v) => tray = Some(v),
                        PrefUpdate::ShowMetricsBar(v) => metrics = Some(v),
                        PrefUpdate::Shortcuts(v) => shortcuts = Some(v),
                    }
                }
                let mut err = None;
                if let Some(w) = width {
                    let res = match w {
                        Some(val) => prefs.set("sidebar_width", &val),
                        None => prefs.remove("sidebar_width"),
                    };
                    if let Err(e) = res {
                        err = Some(e.to_string());
                    }
                }
                if let Some(t) = tray
                    && let Err(e) = prefs.set("close_to_tray", &t)
                {
                    err = Some(e.to_string());
                }
                if let Some(m) = metrics
                    && let Err(e) = prefs.set("show_metrics_bar", &m)
                {
                    err = Some(e.to_string());
                }
                if let Some(s) = shortcuts
                    && let Err(e) = prefs.set("shortcuts", &s)
                {
                    err = Some(e.to_string());
                }
                if let Some(error) = err {
                    let _ = report.send(error);
                    wake();
                }
            }
        });
        Self {
            updates: Some(updates),
            errors,
            worker: Some(worker),
        }
    }
}

impl Drop for PrefsWriter {
    fn drop(&mut self) {
        drop(self.updates.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Unread,
    Running,
    Finished,
}

impl Filter {
    pub const ITEMS: [(Self, &'static str, &'static str); 4] = [
        (Self::All, "all", "全部"),
        (Self::Unread, "unread", "未读"),
        (Self::Running, "running", "进行中"),
        (Self::Finished, "finished", "已完成"),
    ];

    pub fn includes(self, tab: &Tab, selected: bool) -> bool {
        match self {
            Self::All => true,
            Self::Unread => tab.unread && !selected,
            Self::Running => tab.state_text() == "运行中",
            Self::Finished => {
                tab.foreground.is_some()
                    && tab.state_text() == "就绪"
                    && (tab.last_activity_ms.is_some() || tab.prompt_count > 0)
            }
        }
    }
}

pub fn drawer_focused(tx: &Tx) -> bool {
    tx.focused().is_some_and(|key| {
        key.split('/').any(|part| {
            matches!(part, "footprints" | "close-footprints")
                || part.starts_with("prompt-")
                || part.starts_with("copy-prompt-")
        })
    })
}

pub struct Layout {
    pub collapsed: HashSet<PathBuf>,
    pub search_collapsed: HashSet<PathBuf>,
    prefs_path: Option<PathBuf>,
    writer: Option<PrefsWriter>,
    preferred: Option<f32>,
    pub close_to_tray: bool,
    pub show_metrics_bar: bool,
    pub shortcuts: Shortcuts,
    pub recording_shortcut: Option<ShortcutAction>,
    pub shortcut_notice: Option<String>,
    pub viewport: std::cell::Cell<[f32; 2]>,
    pub settings: bool,
    pub search: rune::TextEdit,
    pub filter: Filter,
    drag: Option<SplitDrag>,
    last_window_resize: Option<f64>,
    pending_window_size: Option<(Size, f64)>,
}

impl Layout {
    pub fn finish_width_write(&mut self) {
        drop(self.writer.take());
    }

    pub fn cancel_recording(&mut self) {
        self.recording_shortcut = None;
        self.shortcut_notice = None;
    }

    pub fn new(prefs: Prefs) -> Self {
        let preferred = prefs
            .get::<f32>("sidebar_width")
            .filter(|w| w.is_finite())
            .map(|width| width.clamp(MIN_WIDTH, MAX_WIDTH));
        let close_to_tray = prefs.get::<bool>("close_to_tray").unwrap_or(false);
        let show_metrics_bar = prefs.get::<bool>("show_metrics_bar").unwrap_or(true);
        let shortcuts = prefs.get::<Shortcuts>("shortcuts").unwrap_or_default();
        Self {
            collapsed: HashSet::new(),
            search_collapsed: HashSet::new(),
            prefs_path: prefs.path().map(ToOwned::to_owned),
            writer: None,
            preferred,
            close_to_tray,
            show_metrics_bar,
            shortcuts,
            recording_shortcut: None,
            shortcut_notice: None,
            viewport: std::cell::Cell::new([1040., 640.]),
            settings: false,
            search: Default::default(),
            filter: Filter::All,
            drag: None,
            last_window_resize: None,
            pending_window_size: None,
        }
    }

    pub fn rail(&self) -> f32 {
        (self.viewport.get()[0] * 0.046).round().clamp(46., 76.)
    }

    pub fn width(&self, cell: f32) -> f32 {
        let window = self.viewport.get()[0];
        let floor = (window * 0.30).min(MIN_WIDTH);
        let default = (window * 0.278 - self.rail() - GRIP * 0.5)
            .round()
            .clamp(DEFAULT_WIDTH, MAX_WIDTH);
        self.preferred
            .unwrap_or(default)
            .min((window - self.rail() - GRIP - 16. - 60. * cell).max(floor))
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn queue_split_size(&mut self, target: Size, current: Size) -> bool {
        if let Some(drag) = &mut self.drag {
            self.pending_window_size = None;
            drag.pending_size = (target != current).then_some(target);
            return true;
        }
        false
    }

    pub fn queue_window_size(
        &mut self,
        target: Size,
        current: Size,
        clock: f64,
    ) -> Option<std::time::Duration> {
        if target == current {
            self.pending_window_size = None;
            return None;
        }
        let cooldown = WINDOW_RESIZE_COOLDOWN.as_secs_f64();
        if let Some(last) = self.last_window_resize
            && clock >= last
            && clock - last < cooldown
        {
            let due = last + cooldown;
            self.pending_window_size = Some((target, due));
            return Some(std::time::Duration::from_secs_f64((due - clock).max(0.001)));
        }
        self.last_window_resize = Some(clock);
        self.pending_window_size = None;
        None
    }

    pub fn clear_pending_window_size(&mut self, clock: f64) {
        self.last_window_resize = Some(clock);
        self.pending_window_size = None;
    }

    pub fn collapsed_for_search(&self, searching: bool) -> &HashSet<PathBuf> {
        if searching {
            &self.search_collapsed
        } else {
            &self.collapsed
        }
    }

    pub fn matches_search(&self, tab: &Tab, query: &str) -> bool {
        query.is_empty()
            || tab
                .directory
                .to_string_lossy()
                .to_lowercase()
                .contains(query)
            || tab.label().to_lowercase().contains(query)
            || tab.preview.to_lowercase().contains(query)
    }
}

impl Workspace {
    pub fn poll_split_size(&mut self, clock: f64, tx: &mut Tx) -> bool {
        if let Some(drag) = &mut self.layout.drag {
            let Some(size) = drag.pending_size else {
                return false;
            };
            if clock < drag.next_reflow {
                tx.wake_after(std::time::Duration::from_secs_f64(drag.next_reflow - clock));
                return false;
            }
            drag.pending_size = None;
            self.apply_grid_size(size);
            return true;
        }
        let Some((size, due)) = self.layout.pending_window_size else {
            return false;
        };
        if clock < due {
            tx.wake_after(std::time::Duration::from_secs_f64(due - clock));
            return false;
        }
        self.layout.clear_pending_window_size(clock);
        self.apply_grid_size(size)
    }
    fn save_pref(&mut self, update: PrefUpdate, label: &str) {
        let Some(path) = &self.layout.prefs_path else {
            return;
        };
        let writer = self
            .layout
            .writer
            .get_or_insert_with(|| PrefsWriter::start(path.clone(), self.wake.clone()));
        if let Some(tx) = &writer.updates
            && tx.send(update).is_err()
        {
            self.history.error = format!("{label}未能保存");
        }
    }

    fn save_width(&mut self) {
        let width = self.layout.preferred;
        self.save_pref(PrefUpdate::SidebarWidth(width), "侧栏宽度");
    }

    pub fn set_close_to_tray(&mut self, enabled: bool, tx: &mut Tx) {
        tx.instant();
        self.layout.close_to_tray = enabled;
        self.sync_tray();
        self.save_pref(PrefUpdate::CloseToTray(enabled), "托盘设置");
    }

    pub fn set_show_metrics_bar(&mut self, enabled: bool, tx: &mut Tx) {
        tx.instant();
        self.layout.show_metrics_bar = enabled;
        self.desktop.set_metrics_enabled(enabled);
        self.save_pref(PrefUpdate::ShowMetricsBar(enabled), "状态栏设置");
    }

    pub fn toggle_record_shortcut(&mut self, action: ShortcutAction, tx: &mut Tx) {
        tx.instant();
        if self.layout.recording_shortcut == Some(action) {
            self.layout.cancel_recording();
        } else {
            self.layout.recording_shortcut = Some(action);
            self.layout.shortcut_notice = None;
        }
    }

    pub fn reset_shortcuts(&mut self, tx: &mut Tx) {
        tx.instant();
        self.layout.shortcuts = Shortcuts::default();
        self.layout.cancel_recording();
        let shortcuts = self.layout.shortcuts.clone();
        self.save_pref(PrefUpdate::Shortcuts(shortcuts), "快捷键设置");
    }

    pub fn poll_width_error(&mut self) -> bool {
        let Some(error) = self
            .layout
            .writer
            .as_ref()
            .and_then(|writer| writer.errors.try_iter().last())
        else {
            return false;
        };
        self.history.error = format!("设置未能保存 · {error}");
        true
    }

    fn split_drag(&mut self, d: &mut Drag, tx: &mut Tx) {
        match d.phase {
            Phase::Down => {
                self.layout.drag = Some(SplitDrag {
                    previous_width: self.layout.preferred,
                    pending_size: None,
                    next_reflow: 0.,
                });
                d.data = self.layout.width(self.cell);
                tx.instant();
            }
            Phase::Move if let Some(drag) = &mut self.layout.drag => {
                let next = Some((d.data + d.dx).round().clamp(MIN_WIDTH, MAX_WIDTH));
                if self.layout.preferred == next {
                    tx.live_only();
                    return;
                }
                self.layout.preferred = next;
                tx.instant();
                // Keep columns steady as the divider moves; wrap at a pause or on release.
                drag.next_reflow = tx.clock() + REFLOW_PAUSE.as_secs_f64();
                tx.wake_after(REFLOW_PAUSE);
            }
            Phase::Up => {
                tx.instant();
                if let Some(drag) = self.layout.drag.take() {
                    if d.cancelled() {
                        self.layout.preferred = drag.previous_width;
                    } else {
                        if d.clicks == 2 {
                            self.layout.preferred = None;
                        } else if let Some(size) = drag.pending_size {
                            self.apply_grid_size(size);
                        }
                        if self.layout.preferred != drag.previous_width {
                            self.save_width();
                        }
                        self.select(self.active, tx);
                    }
                }
            }
            _ => (),
        }
    }

    pub fn cancel_split(&mut self) {
        if let Some(drag) = self.layout.drag.take() {
            self.layout.preferred = drag.previous_width;
        }
    }

    pub fn workbench_key(&mut self, e: &KeyEvent, tx: &mut Tx) -> bool {
        if !self.window_active
            || self.composing
            || self.search_composing()
            || self.layout.search.is_composing()
            || e.mods.logo
        {
            return false;
        }
        let once = !tx.key_repeat();
        if self.layout.settings
            && let Some(target) = self.layout.recording_shortcut
        {
            if !once {
                return true;
            }
            if !e.mods.ctrl && !e.mods.shift && !e.mods.alt && e.key == Key::Escape {
                self.layout.cancel_recording();
                return true;
            }
            let Some(candidate) = Shortcut::from_event(e) else {
                self.layout.shortcut_notice =
                    Some("请按下包含 Ctrl / Alt 的组合键或 F1-F12，按 Esc 取消".into());
                return true;
            };
            if candidate.is_reserved_terminal() {
                self.layout.shortcut_notice =
                    Some("Ctrl+C / Ctrl+V 保留给终端复制粘贴，请搭配 Shift 或 Alt".into());
                return true;
            }
            if let Some(other) = self.layout.shortcuts.conflict(target, &candidate) {
                self.layout.shortcut_notice =
                    Some(format!("与「{}」冲突，请换一个组合键", other.label()));
                return true;
            }
            self.layout.shortcuts.set(target, candidate);
            self.layout.cancel_recording();
            let shortcuts = self.layout.shortcuts.clone();
            self.save_pref(PrefUpdate::Shortcuts(shortcuts), "快捷键设置");
            return true;
        }
        let drawer = drawer_focused(tx);
        if let Some(action) = self.layout.shortcuts.match_event(e) {
            if once {
                match action {
                    ShortcutAction::Search => {
                        self.layout.settings = false;
                        self.layout.cancel_recording();
                        self.invalidate_input();
                        tx.focus_when_shown("sidebar-search", true);
                    }
                    ShortcutAction::Footprints => self.toggle_footprints(tx),
                    ShortcutAction::Archive => self.toggle_archive(tx),
                    ShortcutAction::NewShell => {
                        let dir = self
                            .current()
                            .map(|t| t.directory.clone())
                            .filter(|d| !d.as_os_str().is_empty());
                        self.new_shell(dir, tx);
                    }
                    ShortcutAction::PrevTab | ShortcutAction::NextTab => {
                        let ids: Vec<_> = self
                            .groups()
                            .into_iter()
                            .flatten()
                            .filter(|t| !t.closing)
                            .map(|t| t.id)
                            .collect();
                        if let Some(pos) = ids.iter().position(|id| Some(*id) == self.active) {
                            let next = if action == ShortcutAction::NextTab {
                                pos + 1
                            } else {
                                pos + ids.len() - 1
                            };
                            let id = ids[next % ids.len()];
                            self.select(Some(id), tx);
                        }
                    }
                }
            }
        } else if tx
            .focused()
            .is_some_and(|k| k.rsplit('/').next() == Some("sidebar-search"))
            && !e.mods.ctrl
            && !e.mods.shift
            && !e.mods.alt
            && e.key == Key::Escape
        {
            self.layout.search = Default::default();
            self.layout.search_collapsed.clear();
            self.select(self.active, tx);
            if self.session().is_none() {
                tx.focus("nav-projects", true);
            }
        } else if drawer && e.mods.ctrl && !e.mods.shift && !e.mods.alt && e.is_command('f') {
            if once {
                self.focus_footprints(tx);
            }
        } else if drawer && !e.mods.ctrl && !e.mods.shift && !e.mods.alt && e.key == Key::Escape {
            if once {
                self.toggle_footprints(tx);
            }
        } else {
            return false;
        }
        true
    }
}

pub fn separator() -> El {
    row()
        .key("sidebar-divider")
        .role("slider")
        .label("侧栏宽度")
        .help("拖动调整宽度，双击复位。也可用左右方向键微调。")
        .focusable(true)
        .w(GRIP)
        .h_full()
        .center()
        .cursor(Cursor::ResizeColumn)
        .fill(crate::sidebar::ink(crate::sidebar::PANEL))
        .hover(crate::sidebar::ink(0x34506c))
        .child(el().w(1.).h_full().fill(crate::sidebar::ink(0x293545)))
        .on_drag(Workspace::split_drag)
        .on_key(|w: &mut Workspace, e, tx| {
            if e.mods == Default::default() && matches!(e.key, Key::Left | Key::Right) {
                let delta = if e.key == Key::Left { -8. } else { 8. };
                w.layout.preferred =
                    Some((w.layout.width(w.cell) + delta).clamp(MIN_WIDTH, MAX_WIDTH));
                tx.instant();
                w.save_width();
                return true;
            }
            w.workbench_key(e, tx)
        })
}
