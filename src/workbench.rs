use crate::terminal::Size;
use crate::workspace::{Tab, Workspace};
use rune::prefs::Prefs;
use rune::{
    Align, Color, Cursor, Drag, El, Icon, Key, KeyEvent, Phase, Side, Tx, col, el, icon, row, text,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf, sync::mpsc, thread};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextMenu {
    ProxySwitcher,
    Project {
        anchor: String,
        dir: PathBuf,
        searching: bool,
    },
    ProxyGlobalDefault,
    ProxyAgentDefault {
        agent: &'static str,
        anchor: String,
    },
}

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
    Clawbot(crate::clawbot::ClawbotPrefs),
    Proxy(crate::proxy::ProxySettings),
    Sound(crate::sound::SoundPrefs),
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
            let catalog_path = path.parent().map(crate::proxy::catalog_path);
            let mut prefs = Prefs::at(path);
            while let Ok(first) = requests.recv() {
                let mut width = None;
                let mut tray = None;
                let mut metrics = None;
                let mut shortcuts = None;
                let mut clawbot = None;
                let mut proxy = None;
                let mut sound = None;
                for item in std::iter::once(first).chain(requests.try_iter()) {
                    match item {
                        PrefUpdate::SidebarWidth(v) => width = Some(v),
                        PrefUpdate::CloseToTray(v) => tray = Some(v),
                        PrefUpdate::ShowMetricsBar(v) => metrics = Some(v),
                        PrefUpdate::Shortcuts(v) => shortcuts = Some(v),
                        PrefUpdate::Clawbot(v) => clawbot = Some(v),
                        PrefUpdate::Proxy(v) => proxy = Some(v),
                        PrefUpdate::Sound(v) => sound = Some(v),
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
                if let Some(c) = clawbot
                    && let Err(e) = prefs.set("clawbot", &c)
                {
                    err = Some(e.to_string());
                }
                if let Some(p) = proxy {
                    if let Err(e) = prefs.set("proxy", &p) {
                        err = Some(e.to_string());
                    } else if let Some(cat) = &catalog_path
                        && let Err(e) = crate::proxy::write_catalog(cat, &p)
                    {
                        err = Some(e.to_string());
                    }
                }
                if let Some(snd) = sound
                    && let Err(e) = prefs.set("sound", &snd)
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
    pub sound: crate::sound::SoundPrefs,
    pub proxy: crate::proxy::ProxySettings,
    pub proxy_name_input: rune::TextEdit,
    pub proxy_url_input: rune::TextEdit,
    pub proxy_no_proxy_input: rune::TextEdit,
    pub proxy_notice: Option<String>,
    pub clawbot: crate::clawbot::ClawbotPrefs,
    pub clawbot_base_url_input: rune::TextEdit,
    pub clawbot_default_dir_input: rune::TextEdit,
    pub shortcuts: Shortcuts,
    pub recording_shortcut: Option<ShortcutAction>,
    pub shortcut_notice: Option<String>,
    pub viewport: std::cell::Cell<[f32; 2]>,
    pub settings: bool,
    pub search: rune::TextEdit,
    pub filter: Filter,
    pub menu: Option<ContextMenu>,
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
        let sound = prefs
            .get::<crate::sound::SoundPrefs>("sound")
            .unwrap_or_default();
        let proxy = prefs
            .get::<crate::proxy::ProxySettings>("proxy")
            .unwrap_or_default();
        let prefs_path = prefs.path().map(ToOwned::to_owned);
        if let Some(cat) = prefs_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(crate::proxy::catalog_path)
            && !cat.exists()
        {
            let _ = crate::proxy::write_catalog(&cat, &proxy);
        }
        let clawbot = prefs
            .get::<crate::clawbot::ClawbotPrefs>("clawbot")
            .unwrap_or_default();
        let clawbot_base_url_input = rune::TextEdit::new(clawbot.base_url.clone());
        let clawbot_default_dir_input = rune::TextEdit::new(
            clawbot
                .default_project
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        let shortcuts = prefs.get::<Shortcuts>("shortcuts").unwrap_or_default();
        Self {
            collapsed: HashSet::new(),
            search_collapsed: HashSet::new(),
            prefs_path,
            writer: None,
            preferred,
            close_to_tray,
            show_metrics_bar,
            sound,
            proxy,
            proxy_name_input: Default::default(),
            proxy_url_input: Default::default(),
            proxy_no_proxy_input: Default::default(),
            proxy_notice: None,
            clawbot,
            clawbot_base_url_input,
            clawbot_default_dir_input,
            shortcuts,
            recording_shortcut: None,
            shortcut_notice: None,
            viewport: std::cell::Cell::new([1040., 640.]),
            settings: false,
            search: Default::default(),
            filter: Filter::All,
            menu: None,
            drag: None,
            last_window_resize: None,
            pending_window_size: None,
        }
    }

    pub fn proxy_catalog_path(&self) -> Option<PathBuf> {
        self.prefs_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(crate::proxy::catalog_path)
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

    pub fn set_sound_mode(&mut self, mode: crate::sound::SoundMode, tx: &mut Tx) {
        tx.instant();
        self.layout.sound.mode = mode;
        let sound = self.layout.sound.clone();
        self.save_pref(PrefUpdate::Sound(sound), "提示音设置");
    }

    pub fn set_sound_preset(&mut self, preset: crate::sound::SoundPreset, tx: &mut Tx) {
        tx.instant();
        self.layout.sound.preset = preset;
        let sound = self.layout.sound.clone();
        self.save_pref(PrefUpdate::Sound(sound), "提示音设置");
        crate::sound::preview(preset);
    }

    pub fn set_sound_on_waiting(&mut self, enabled: bool, tx: &mut Tx) {
        tx.instant();
        self.layout.sound.on_waiting = enabled;
        let sound = self.layout.sound.clone();
        self.save_pref(PrefUpdate::Sound(sound), "提示音设置");
    }

    pub fn preview_sound(&mut self, tx: &mut Tx) {
        tx.instant();
        crate::sound::preview(self.layout.sound.preset);
    }

    pub fn save_clawbot_prefs(&mut self) {
        let prefs = self.layout.clawbot.clone();
        self.save_pref(PrefUpdate::Clawbot(prefs), "微信 ClawBot 设置");
    }

    pub fn save_proxy_settings(&mut self) {
        let proxy = self.layout.proxy.clone();
        if let Some(cat) = self.proxy_catalog_path() {
            let _ = crate::proxy::write_catalog(&cat, &proxy);
        }
        self.save_pref(PrefUpdate::Proxy(proxy), "代理预设");
    }

    pub fn add_proxy_profile(&mut self, tx: &mut Tx) {
        tx.instant();
        let name = self.layout.proxy_name_input.text.clone();
        let url = self.layout.proxy_url_input.text.clone();
        let no_proxy = self.layout.proxy_no_proxy_input.text.clone();
        match self.layout.proxy.upsert_profile(&name, &url, &no_proxy) {
            Ok(saved) => {
                self.layout.proxy_name_input = Default::default();
                self.layout.proxy_url_input = Default::default();
                self.layout.proxy_no_proxy_input = Default::default();
                self.layout.proxy_notice = Some(format!(
                    "已保存预设「{}」-> {}",
                    saved.name,
                    crate::proxy::sanitize_proxy_summary(&saved.url)
                ));
                self.save_proxy_settings();
            }
            Err(err) => {
                self.layout.proxy_notice = Some(err);
            }
        }
    }

    pub fn remove_proxy_profile(&mut self, name: String, tx: &mut Tx) {
        tx.instant();
        if self.layout.proxy.remove_profile(&name) {
            self.layout.proxy_notice = Some(format!("已删除代理预设「{name}」"));
            self.save_proxy_settings();
        }
    }

    pub fn set_default_proxy_profile(&mut self, value: Option<String>, tx: &mut Tx) {
        tx.instant();
        self.layout.proxy.default_profile = value;
        self.layout.proxy_notice = None;
        self.save_proxy_settings();
    }

    pub fn set_agent_default_proxy(
        &mut self,
        agent: &'static str,
        value: Option<String>,
        tx: &mut Tx,
    ) {
        tx.instant();
        self.layout.proxy.set_agent_default(agent, value);
        self.layout.proxy_notice = None;
        self.save_proxy_settings();
    }

    pub fn switch_active_tab_proxy(&mut self, target: crate::proxy::ProxyTarget, tx: &mut Tx) {
        tx.instant();
        let Some(index) = self.active_index() else {
            return;
        };
        if self.tabs[index].closing || self.tabs[index].ended.is_some() {
            return;
        }
        let id = self.tabs[index].id;
        if self.tabs[index].foreground.is_none() {
            if let Some(session) = &self.tabs[index].session {
                let cmd = format!("mp {}\r", target.shell_arg());
                session.input(cmd.into_bytes());
            }
            self.tabs[index].proxy_summary = target.initial_summary();
            return;
        }
        if self.tabs[index].state_text() == "运行中" {
            self.history.error = "当前 Agent 正在运行中，请等待完成或先中断后再切换代理。".into();
            return;
        }
        if let Err(err) = self.reopen_tab_with_proxy(id, &target, tx) {
            self.history.error = format!("切换代理失败 · {err}");
        }
    }

    pub fn set_clawbot_enabled(&mut self, enabled: bool, tx: &mut Tx) {
        tx.instant();
        self.layout.clawbot.enabled = enabled;
        self.save_clawbot_prefs();
        self.sync_clawbot_prefs();
    }

    pub fn set_clawbot_allow_raw_shell(&mut self, enabled: bool, tx: &mut Tx) {
        tx.instant();
        self.layout.clawbot.allow_raw_shell = enabled;
        self.save_clawbot_prefs();
    }

    pub fn set_clawbot_notify_desktop_tasks(&mut self, enabled: bool, tx: &mut Tx) {
        tx.instant();
        self.layout.clawbot.notify_desktop_tasks = enabled;
        self.save_clawbot_prefs();
    }

    pub fn set_clawbot_keepalive_remind(&mut self, enabled: bool, tx: &mut Tx) {
        tx.instant();
        self.layout.clawbot.keepalive_remind = enabled;
        self.save_clawbot_prefs();
        self.sync_clawbot_prefs();
    }

    pub fn set_clawbot_default_agent(&mut self, agent: Option<String>, tx: &mut Tx) {
        tx.instant();
        self.layout.clawbot.default_agent = agent;
        self.save_clawbot_prefs();
    }

    pub fn commit_clawbot_base_url(&mut self, tx: &mut Tx) {
        tx.instant();
        let trimmed = self.layout.clawbot_base_url_input.text.trim().to_owned();
        let next = if trimmed.is_empty() {
            crate::clawbot::DEFAULT_BASE_URL.to_owned()
        } else {
            trimmed
        };
        if self.layout.clawbot.base_url != next {
            self.layout.clawbot.base_url = next.clone();
            self.layout.clawbot_base_url_input = rune::TextEdit::new(next);
            self.save_clawbot_prefs();
            self.sync_clawbot_prefs();
        }
    }

    pub fn commit_clawbot_default_dir(&mut self, tx: &mut Tx) {
        tx.instant();
        let trimmed = self.layout.clawbot_default_dir_input.text.trim();
        let next = (!trimmed.is_empty()).then(|| PathBuf::from(trimmed));
        if self.layout.clawbot.default_project != next {
            self.layout.clawbot.default_project = next;
            self.save_clawbot_prefs();
        }
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
            || self.layout.proxy_name_input.is_composing()
            || self.layout.proxy_url_input.is_composing()
            || self.layout.proxy_no_proxy_input.is_composing()
            || self.layout.clawbot_base_url_input.is_composing()
            || self.layout.clawbot_default_dir_input.is_composing()
            || e.mods.logo
        {
            return false;
        }
        let once = !tx.key_repeat();
        if self.layout.menu.is_some()
            && !e.mods.ctrl
            && !e.mods.shift
            && !e.mods.alt
            && e.key == Key::Escape
        {
            if once {
                tx.instant();
                self.layout.menu = None;
            }
            return true;
        }
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

fn menu_divider() -> El {
    el().w_full()
        .h(1.)
        .my(3.)
        .fill(crate::sidebar::ink(0x233247))
}

fn menu_section_header(title: impl Into<String>, sub: Option<String>) -> El {
    let mut header = col().w_full().px(9.).pt(4.).pb(5.).gap(2.).child(
        text(title.into())
            .font(crate::sidebar::ui_font(11.5, 600.))
            .truncate()
            .color(crate::sidebar::ink(0x8ba0ba)),
    );
    if let Some(detail) = sub.filter(|s| !s.is_empty()) {
        header = header.child(
            text(detail)
                .font(crate::sidebar::ui_font(10.5, 400.))
                .truncate()
                .color(crate::sidebar::ink(0x63768f)),
        );
    }
    header
}

fn menu_action_row(
    key: impl Into<String>,
    glyph: &'static Icon,
    glyph_color: u32,
    label: impl Into<String>,
    badge: Option<String>,
    selected: bool,
    on_press: impl Fn(&mut Workspace, &mut Tx) + 'static,
) -> El {
    let label_str = label.into();
    let mut item = row()
        .key(key)
        .role("menuitem")
        .label(label_str.clone())
        .focusable(true)
        .w_full()
        .h(30.)
        .px(9.)
        .gap(8.)
        .items_center()
        .radius(6.)
        .fill(if selected {
            crate::sidebar::ink(0x1a304d)
        } else {
            Color::TRANSPARENT
        })
        .border(
            1.,
            if selected {
                crate::sidebar::ink(0x3469a4)
            } else {
                Color::TRANSPARENT
            },
        )
        .hover(crate::sidebar::ink(if selected {
            0x213c5f
        } else {
            0x1d2b3f
        }))
        .on_press_tx(move |w: &mut Workspace, tx| {
            tx.instant();
            w.layout.menu = None;
            on_press(w, tx);
        })
        .child(icon(glyph, 14.).color(crate::sidebar::ink(if selected {
            0x6dc2ff
        } else {
            glyph_color
        })))
        .child(
            text(label_str)
                .font(crate::sidebar::ui_font(
                    12.5,
                    if selected { 580. } else { 460. },
                ))
                .grow(1.)
                .min_w(0.)
                .truncate()
                .color(crate::sidebar::ink(if selected {
                    0xf2f7ff
                } else {
                    0xdbe4f0
                })),
        );
    if let Some(b) = badge.filter(|s| !s.is_empty()) {
        item = item.child(
            row()
                .h(18.)
                .px(6.)
                .center()
                .radius(4.)
                .fill(crate::sidebar::ink(if selected {
                    0x244872
                } else {
                    0x1d2838
                }))
                .child(text(b).font(crate::sidebar::ui_font(10.5, 520.)).color(
                    crate::sidebar::ink(if selected { 0xbfe2ff } else { 0x8ea4be }),
                )),
        );
    }
    item
}

fn menu_surface(key: &str, width: f32, content: El) -> El {
    col()
        .key(key)
        .role("menu")
        .w(width)
        .padding(5.)
        .gap(2.)
        .radius(10.)
        .fill(crate::sidebar::ink(0x121b28))
        .border(1., crate::sidebar::ink(0x2e415a))
        .shadow(10., 26., Color::BLACK.alpha(0.65))
        .shadow(2., 8., Color::BLACK.alpha(0.42))
        .modal_tx(true, |w: &mut Workspace, tx| {
            tx.instant();
            w.layout.menu = None;
        })
        .child(content)
}

pub fn menu_overlay(w: &Workspace) -> Vec<El> {
    let Some(menu) = &w.layout.menu else {
        return Vec::new();
    };
    let scrim = el()
        .key("mcli-menu-scrim")
        .role("none")
        .absolute(0., 0.)
        .w_full()
        .h_full()
        .on_press_tx(|w: &mut Workspace, tx| {
            tx.instant();
            w.layout.menu = None;
        });

    let popup = match menu {
        ContextMenu::ProxySwitcher => {
            use crate::proxy::{ProxyTarget, SummaryMatch, short_host_port};
            let current = w
                .current()
                .map(|t| w.layout.proxy.match_summary(&t.proxy_summary))
                .unwrap_or(SummaryMatch::System(None));
            let is_agent = w.current().is_some_and(|t| t.foreground.is_some());
            let mut list = col()
                .w_full()
                .gap(2.)
                .child(menu_section_header(
                    "切换当前终端代理",
                    Some(if is_agent {
                        "Agent 终端将平滑重开并续接原对话".into()
                    } else {
                        current.detail_label()
                    }),
                ))
                .child(menu_action_row(
                    "menu-item-sys",
                    if matches!(current, SummaryMatch::System(_)) {
                        &crate::sidebar::icons::CHECK
                    } else {
                        &crate::sidebar::icons::GLOBE
                    },
                    0x8ea4be,
                    "系统默认",
                    match &current {
                        SummaryMatch::System(Some(url)) => Some(short_host_port(url)),
                        _ => None,
                    },
                    matches!(current, SummaryMatch::System(_)),
                    |w, tx| w.switch_active_tab_proxy(ProxyTarget::System, tx),
                ))
                .child(menu_action_row(
                    "menu-item-off",
                    if matches!(current, SummaryMatch::Direct) {
                        &crate::sidebar::icons::CHECK
                    } else {
                        &crate::sidebar::icons::ONLINE
                    },
                    0x5bd49a,
                    "直连（清除代理）",
                    Some("OFF".into()),
                    matches!(current, SummaryMatch::Direct),
                    |w, tx| w.switch_active_tab_proxy(ProxyTarget::Direct, tx),
                ));
            if !w.layout.proxy.profiles.is_empty() {
                list = list.child(menu_divider());
                for p in &w.layout.proxy.profiles {
                    let is_active = matches!(
                        &current,
                        SummaryMatch::Profile { name, .. } if name.eq_ignore_ascii_case(&p.name)
                    );
                    let prof = p.clone();
                    list = list.child(menu_action_row(
                        format!("menu-item-profile-{}", p.name),
                        if is_active {
                            &crate::sidebar::icons::CHECK
                        } else {
                            &crate::sidebar::icons::GLOBE
                        },
                        0x69b8ff,
                        p.name.clone(),
                        Some(short_host_port(&p.url)),
                        is_active,
                        move |w, tx| {
                            w.switch_active_tab_proxy(ProxyTarget::Profile(prof.clone()), tx)
                        },
                    ));
                }
            }
            list = list.child(menu_divider()).child(menu_action_row(
                "menu-item-manage",
                &crate::sidebar::icons::SETTINGS,
                0x9eb2c9,
                "管理代理预设…",
                None,
                false,
                |w, tx| {
                    w.layout.settings = true;
                    w.layout.cancel_recording();
                    tx.focus_when_shown("proxy-name-input", true);
                },
            ));
            menu_surface("mcli-context-menu", 236., list).anchored_fit(
                "proxy-switcher",
                Side::Bottom,
                Align::Start,
                5.,
            )
        }
        ContextMenu::Project {
            anchor,
            dir,
            searching,
        } => {
            let searching = *searching;
            let norm = crate::sidebar::normalize_explorer_path(dir);
            let dir_name = norm
                .file_name()
                .unwrap_or(norm.as_os_str())
                .to_string_lossy()
                .into_owned();
            let dir_display = norm.display().to_string();
            let mut list = col().w_full().gap(2.);
            if !norm.as_os_str().is_empty() {
                let d_open = norm.clone();
                let d_shell = norm.clone();
                let d_copy = dir_display.clone();
                list = list
                    .child(menu_section_header(
                        if dir_name.is_empty() {
                            "项目目录".to_owned()
                        } else {
                            dir_name
                        },
                        Some(dir_display),
                    ))
                    .child(menu_action_row(
                        "menu-item-open-dir",
                        &crate::sidebar::icons::FOLDER,
                        0x8ec5ff,
                        "在资源管理器中打开目录",
                        None,
                        false,
                        move |w, _| crate::sidebar::open_in_explorer(w, &d_open),
                    ))
                    .child(menu_action_row(
                        "menu-item-new-shell",
                        &crate::sidebar::icons::TERMINAL,
                        0x7fdca8,
                        "在此目录新建终端",
                        None,
                        false,
                        move |w, tx| w.new_shell(Some(d_shell.clone()), tx),
                    ))
                    .child(menu_action_row(
                        "menu-item-copy-path",
                        &crate::sidebar::icons::COPY,
                        0x9eb2c9,
                        "复制目录路径",
                        None,
                        false,
                        move |_, tx| tx.copy(d_copy.clone()),
                    ))
                    .child(menu_divider());
            }
            list = list
                .child(menu_action_row(
                    "menu-item-expand-all",
                    &crate::sidebar::icons::EXPAND_ALL,
                    0x9eb2c9,
                    "展开所有项目目录",
                    None,
                    false,
                    move |w, _| {
                        if searching {
                            w.layout.search_collapsed.clear();
                        } else {
                            w.layout.collapsed.clear();
                        }
                    },
                ))
                .child(menu_action_row(
                    "menu-item-collapse-all",
                    &crate::sidebar::icons::COLLAPSE_ALL,
                    0x9eb2c9,
                    "折叠所有项目目录",
                    None,
                    false,
                    move |w, _| {
                        let projects: Vec<_> = w
                            .tabs
                            .iter()
                            .filter(|t| !t.closing)
                            .map(|t| t.project.clone())
                            .collect();
                        let set = if searching {
                            &mut w.layout.search_collapsed
                        } else {
                            &mut w.layout.collapsed
                        };
                        set.extend(projects);
                    },
                ));
            menu_surface("mcli-context-menu", 224., list).anchored_fit(
                anchor.as_str(),
                Side::Bottom,
                Align::Start,
                4.,
            )
        }
        ContextMenu::ProxyGlobalDefault => {
            let cur = w.layout.proxy.default_profile.as_deref();
            let mut list = col()
                .w_full()
                .gap(2.)
                .child(menu_section_header("新终端默认代理", None))
                .child(menu_action_row(
                    "menu-item-default-sys",
                    if cur.is_none() {
                        &crate::sidebar::icons::CHECK
                    } else {
                        &crate::sidebar::icons::GLOBE
                    },
                    0x8ea4be,
                    "系统默认",
                    None,
                    cur.is_none(),
                    |w, tx| w.set_default_proxy_profile(None, tx),
                ))
                .child(menu_action_row(
                    "menu-item-default-direct",
                    if cur == Some(crate::proxy::DIRECT_PROFILE) {
                        &crate::sidebar::icons::CHECK
                    } else {
                        &crate::sidebar::icons::ONLINE
                    },
                    0x5bd49a,
                    "直连（清除代理）",
                    Some("OFF".into()),
                    cur == Some(crate::proxy::DIRECT_PROFILE),
                    |w, tx| {
                        w.set_default_proxy_profile(
                            Some(crate::proxy::DIRECT_PROFILE.to_owned()),
                            tx,
                        )
                    },
                ));
            if !w.layout.proxy.profiles.is_empty() {
                list = list.child(menu_divider());
                for p in &w.layout.proxy.profiles {
                    let selected = cur.is_some_and(|c| c.eq_ignore_ascii_case(&p.name));
                    let name = p.name.clone();
                    list = list.child(menu_action_row(
                        format!("menu-item-default-{}", p.name),
                        if selected {
                            &crate::sidebar::icons::CHECK
                        } else {
                            &crate::sidebar::icons::GLOBE
                        },
                        0x69b8ff,
                        p.name.clone(),
                        Some(crate::proxy::short_host_port(&p.url)),
                        selected,
                        move |w, tx| w.set_default_proxy_profile(Some(name.clone()), tx),
                    ));
                }
            }
            menu_surface("mcli-context-menu", 216., list).anchored_fit(
                "proxy-global-default",
                Side::Bottom,
                Align::End,
                4.,
            )
        }
        ContextMenu::ProxyAgentDefault { agent, anchor } => {
            let agent = *agent;
            let cur = w.layout.proxy.agent_default(agent);
            let mut list = col()
                .w_full()
                .gap(2.)
                .child(menu_section_header(
                    format!("启动 {agent} 时的自动代理"),
                    None,
                ))
                .child(menu_action_row(
                    "menu-item-agent-inherit",
                    if cur.is_none() {
                        &crate::sidebar::icons::CHECK
                    } else {
                        &crate::sidebar::icons::GLOBE
                    },
                    0x8ea4be,
                    "跟随终端",
                    None,
                    cur.is_none(),
                    move |w, tx| w.set_agent_default_proxy(agent, None, tx),
                ))
                .child(menu_action_row(
                    "menu-item-agent-direct",
                    if cur == Some(crate::proxy::DIRECT_PROFILE) {
                        &crate::sidebar::icons::CHECK
                    } else {
                        &crate::sidebar::icons::ONLINE
                    },
                    0x5bd49a,
                    "直连（清除代理）",
                    Some("OFF".into()),
                    cur == Some(crate::proxy::DIRECT_PROFILE),
                    move |w, tx| {
                        w.set_agent_default_proxy(
                            agent,
                            Some(crate::proxy::DIRECT_PROFILE.to_owned()),
                            tx,
                        )
                    },
                ));
            if !w.layout.proxy.profiles.is_empty() {
                list = list.child(menu_divider());
                for p in &w.layout.proxy.profiles {
                    let selected = cur.is_some_and(|c| c.eq_ignore_ascii_case(&p.name));
                    let name = p.name.clone();
                    list = list.child(menu_action_row(
                        format!("menu-item-agent-{}", p.name),
                        if selected {
                            &crate::sidebar::icons::CHECK
                        } else {
                            &crate::sidebar::icons::GLOBE
                        },
                        0x69b8ff,
                        p.name.clone(),
                        Some(crate::proxy::short_host_port(&p.url)),
                        selected,
                        move |w, tx| w.set_agent_default_proxy(agent, Some(name.clone()), tx),
                    ));
                }
            }
            menu_surface("mcli-context-menu", 216., list).anchored_fit(
                anchor.as_str(),
                Side::Bottom,
                Align::End,
                4.,
            )
        }
    };

    vec![scrim, popup]
}
