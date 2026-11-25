use crate::pty;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor, Rgb};
use rune::Waker;
use std::io::{self, Read, Write};
use std::os::windows::io::OwnedHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub lines: u16,
    pub cell_w: u16,
    pub cell_h: u16,
}
impl Default for Size {
    fn default() -> Self {
        Self {
            cols: 80,
            lines: 24,
            cell_w: 9,
            cell_h: 20,
        }
    }
}
impl Dimensions for Size {
    fn columns(&self) -> usize {
        self.cols.into()
    }
    fn screen_lines(&self) -> usize {
        self.lines.into()
    }
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
}
impl From<Size> for WindowSize {
    fn from(s: Size) -> Self {
        Self {
            num_cols: s.cols,
            num_lines: s.lines,
            cell_width: s.cell_w,
            cell_height: s.cell_h,
        }
    }
}

pub fn palette(i: usize) -> Rgb {
    // Windows Terminal's Campbell palette, in ANSI (not Win32 COLORREF) order.
    const ANSI: [u32; 16] = [
        0x0c0c0c, 0xc50f1f, 0x13a10e, 0xc19c00, 0x0037da, 0x881798, 0x3a96dd, 0xcccccc, 0x767676,
        0xe74856, 0x16c60c, 0xf9f1a5, 0x3b78ff, 0xb4009e, 0x61d6d6, 0xf2f2f2,
    ];
    let n = match i {
        0..=15 => ANSI[i],
        16..=231 => {
            let v = i - 16;
            let level = |n: usize| if n == 0 { 0 } else { 55 + 40 * n as u32 };
            level(v / 36) << 16 | level(v / 6 % 6) << 8 | level(v % 6)
        }
        232..=255 => (8 + (i as u32 - 232) * 10) * 0x010101,
        257 => ANSI[0],
        258 => 0xffffff,
        259..=266 => (ANSI[i - 259] >> 1) & 0x7f7f7f,
        _ => ANSI[7],
    };
    Rgb {
        r: (n >> 16) as u8,
        g: (n >> 8) as u8,
        b: n as u8,
    }
}

enum Input {
    Data(Vec<u8>),
    Resize(Size),
    Stop,
}
#[derive(Default)]
pub struct ShellUpdates {
    pub restore_end: Option<crate::shell::RestoreEnd>,
    pub launch: Option<String>,
    pub explicit_id: Option<String>,
    pub tui_ready: bool,
    pub agy_events: Vec<(String, u64, crate::shell::AgyEvent)>,
    pub status: Option<(crate::shell::Foreground, &'static str)>,
    pub screen_state: Option<Option<&'static str>>,
    pub directory: Option<PathBuf>,
    pub title: Option<String>,
    pub foreground: Option<Option<crate::shell::Foreground>>,
    pub proxy_summary: Option<String>,
}

struct RestoreSignals {
    claimed: OwnedHandle,
    clean_exit: OwnedHandle,
    bound: bool,
}

impl RestoreSignals {
    fn open(marker: &str) -> io::Result<Self> {
        let name = format!(r"Local\{marker}-restore-");
        Ok(Self {
            claimed: pty::open_signal(&format!("{name}claimed"))?,
            clean_exit: pty::open_signal(&format!("{name}clean-exit"))?,
            bound: false,
        })
    }
}
impl ShellUpdates {
    fn agy_event(&mut self, run: &str, event: crate::shell::AgyEvent) {
        // Coalesce identical consecutive status reports in one frame while preserving
        // distinct OSC 7501 state transitions (such as working -> done) and thread switches.
        if let Some((last_run, sent, kind)) = self.agy_events.last_mut()
            && last_run == run
            && match (&*kind, &event) {
                (crate::shell::AgyEvent::Status(_), crate::shell::AgyEvent::Status(_)) => true,
                (
                    crate::shell::AgyEvent::ProgramStatus(a),
                    crate::shell::AgyEvent::ProgramStatus(b),
                ) => a == b,
                (crate::shell::AgyEvent::ComposerIdle, crate::shell::AgyEvent::ComposerIdle) => {
                    true
                }
                _ => false,
            }
        {
            *sent = crate::hook_wire::timestamp_micros();
            *kind = event;
            return;
        }
        self.agy_events
            .push((run.to_owned(), crate::hook_wire::timestamp_micros(), event));
    }
}
#[derive(Clone, Copy, Debug)]
struct ResizeSettle {
    deadline: std::time::Instant,
    quiet_until: Option<std::time::Instant>,
}

const RESIZE_MAX_WAIT: std::time::Duration = std::time::Duration::from_millis(65);
const RESIZE_QUIET_GAP: std::time::Duration = std::time::Duration::from_millis(12);

#[derive(Clone)]
pub struct Listener {
    input: mpsc::Sender<Input>,
    restore: Arc<Mutex<Option<RestoreSignals>>>,
    size: Arc<Mutex<Size>>,
    resize_settle: Arc<Mutex<Option<ResizeSettle>>>,
    pub dirty: Arc<AtomicBool>,
    pub visible: Arc<AtomicBool>,
    pub sidebar_dirty: Arc<AtomicBool>,
    pub shell: Arc<Mutex<ShellUpdates>>,
    last_title: Arc<Mutex<Option<String>>>,
    wake: Waker,
}
impl Listener {
    fn refresh(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) && self.visible.load(Ordering::Acquire) {
            (self.wake)();
        }
    }
    fn notify_sidebar(&self) {
        if !self.sidebar_dirty.swap(true, Ordering::AcqRel) {
            (self.wake)();
        }
    }
    fn reply(&self, s: String) {
        let _ = self.input.send(Input::Data(s.into_bytes()));
    }
}
impl EventListener for Listener {
    fn send_event(&self, e: Event) {
        match e {
            Event::Title(title) => {
                let mut last = self.last_title.lock().unwrap();
                if last.as_deref() != Some(title.as_str()) {
                    *last = Some(title.clone());
                    drop(last);
                    self.shell.lock().unwrap().title = Some(title);
                    self.notify_sidebar();
                }
            }
            Event::ResetTitle => {
                let mut last = self.last_title.lock().unwrap();
                if last.as_deref() != Some("") {
                    *last = Some(String::new());
                    drop(last);
                    self.shell.lock().unwrap().title = Some(String::new());
                    self.notify_sidebar();
                }
            }
            Event::PtyWrite(s) => self.reply(s),
            Event::ColorRequest(i, f) => self.reply(f(palette(i))),
            Event::TextAreaSizeRequest(f) => self.reply(f((*self.size.lock().unwrap()).into())),
            // A program querying OSC 52 receives an empty clipboard. Paste is a user action.
            Event::ClipboardLoad(_, f) => self.reply(f("")),
            _ => (),
        }
    }
}

// Some native prompts and provider errors only appear in the live viewport.
// The stock agy composer puts its activity hint below the cursor's input row
// and separator. A matching phrase in conversation text has no status meaning.
pub(super) fn agy_composer_busy<T>(term: &Term<T>) -> Option<bool> {
    let row = term.grid().cursor.point.line.0;
    let width = term.columns();
    if row < 1 || row + 2 >= term.screen_lines() as i32 || width < 16 {
        return None;
    }
    let cell = |y, x| term.grid()[Line(y)][Column(x)].c;
    if cell(row, 0) != '>' || cell(row, 1) != ' ' {
        return None;
    }
    for y in [row - 1, row + 1] {
        if ![0, 1, width / 2].into_iter().all(|x| cell(y, x) == '─') {
            return None;
        }
    }
    let hint = |s: &str| {
        s.chars().enumerate().all(|(x, c)| cell(row + 2, x) == c)
            && cell(row + 2, s.len()).is_whitespace()
    };
    if hint("esc to cancel") {
        Some(true)
    } else if hint("? for shortcuts") {
        Some(false)
    } else {
        None
    }
}

pub(super) fn tui_ready<T>(term: &Term<T>, agent: crate::shell::Foreground) -> bool {
    if agent == crate::shell::Foreground::Agy {
        if agy_composer_busy(term).is_some() {
            return true;
        }
    } else if term.mode().contains(TermMode::ALT_SCREEN) {
        return true;
    }
    let end = term.screen_lines() as i32;
    let start = (end - 28).max(0);
    let columns = term.columns().min(240);
    let mut line = String::with_capacity(columns);
    for y in start..end {
        line.clear();
        let cells = &term.grid()[Line(y)];
        line.extend((0..columns).map(|x| cells[Column(x)].c));
        match agent {
            crate::shell::Foreground::Agy => {
                if (line.contains("mode:") && line.contains("shift+tab"))
                    || line.contains("? for shortcuts")
                {
                    return true;
                }
            }
            crate::shell::Foreground::Codex => {
                if line.contains("? for shortcuts")
                    || line.contains("context left")
                    || line.contains("OpenAI Codex")
                    || line.contains("tab to queue message")
                    || line.contains("Update available")
                {
                    return true;
                }
            }
            crate::shell::Foreground::Kimi => {
                if line.contains("Kimi Code")
                    || line.contains("? for shortcuts")
                    || (line.contains("shift+tab") && line.contains("mode"))
                {
                    return true;
                }
            }
            crate::shell::Foreground::Grok => {
                if line.contains("Grok")
                    || line.contains("? for shortcuts")
                    || line.contains("/resume")
                {
                    return true;
                }
            }
        }
    }
    false
}

pub(super) fn screen_state<T>(
    term: &Term<T>,
    agent: crate::shell::Foreground,
) -> Option<&'static str> {
    let end = term.screen_lines() as i32;
    let start = (end - 40).max(0);
    let columns = term.columns();
    let selected = (start..end).any(|y| {
        let cells = &term.grid()[Line(y)];
        let mut left = (0..columns.min(18))
            .map(|x| cells[Column(x)].c)
            .skip_while(|c| c.is_whitespace() || *c == '│');
        matches!(left.next(), Some('>' | '›' | '❯'))
            && left.next() == Some(' ')
            && left.next().is_some_and(|c| c.is_ascii_digit())
            && left.next() == Some('.')
    });
    if agent == crate::shell::Foreground::Agy && columns >= 10 {
        let line = |y| {
            let cells = &term.grid()[Line(y)];
            (0..columns.min(240))
                .map(|x| cells[Column(x)].c)
                .collect::<String>()
        };
        for y in (start + 1..end).rev() {
            let cells = &term.grid()[Line(y)];
            if cells[Column(0)].c != 'E' || cells[Column(1)].c != 'r' {
                continue;
            }
            let current = line(y);
            let Some(id) = current.trim().strip_prefix("Error ID: ") else {
                continue;
            };
            if !id.split_at_checked(36).is_some_and(|(uuid, rest)| {
                crate::hook_wire::valid_id(uuid)
                    && rest
                        .strip_prefix('-')
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            }) {
                continue;
            }
            let fault = (start.max(y - 3)..y).rev().any(|previous| {
                let previous = line(previous);
                previous.trim_start().starts_with('⚠') && previous.contains(" (code ")
            });
            let composer_only = (y + 1..end).all(|next| {
                let cells = &term.grid()[Line(next)];
                if cells[Column(0)].c != '>' {
                    return true;
                }
                let next = line(next);
                let next = next.trim();
                next == ">" || next.contains("mode:") && next.contains("shift+tab")
            });
            if fault && composer_only && !selected {
                return Some("出错");
            }
            break;
        }
    }
    if !selected {
        return None;
    }
    let mut lines = String::new();
    for y in start..end {
        let cells = &term.grid()[Line(y)];
        lines.extend((0..columns.min(240)).map(|x| cells[Column(x)].c));
        lines.push('\n');
    }
    let waiting = match agent {
        crate::shell::Foreground::Agy => {
            lines.contains("No, cancel")
                && lines.contains("Navigate")
                && (lines.contains("Run this command?")
                    || lines.contains("Requesting permission for:"))
        }
        crate::shell::Foreground::Codex => {
            lines.contains("Yes, proceed")
                && (lines.contains("esc to cancel") || lines.contains("No, and tell Codex"))
        }
        crate::shell::Foreground::Kimi | crate::shell::Foreground::Grok => false,
    };
    waiting.then_some("等待用户")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Code(u32),
    Closed,
    Error(String),
}

/// ConPTY serializes double underline (SGR 4:2) as its legacy SGR 21.
/// vte 0.15 maps 21 to "cancel bold". Restart just that CSI with the unambiguous spelling.
#[derive(Default)]
pub(crate) struct ConptySgr {
    matched: usize,
}

impl ConptySgr {
    pub(crate) fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(&[u8])) {
        const LEGACY: &[u8] = b"\x1b[21m";
        let mut start = 0;
        let mut pos = 0;
        while pos < bytes.len() {
            if self.matched == 0 {
                let Some(next) = bytes[pos..].iter().position(|b| *b == 0x1b) else {
                    break;
                };
                pos += next;
            }
            let byte = bytes[pos];
            if byte == LEGACY[self.matched] {
                self.matched += 1;
                if self.matched == LEGACY.len() {
                    // The VT parser has seen (or is about to see) ESC [ 2 1, but not its final m.
                    // ESC starts a new CSI even when the original prefix arrived in a previous read.
                    emit(&bytes[start..pos]);
                    emit(b"\x1b[4:2m");
                    self.matched = 0;
                    start = pos + 1;
                }
            } else {
                self.matched = usize::from(byte == 0x1b);
            }
            pos += 1;
        }
        if start < bytes.len() {
            emit(&bytes[start..]);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SessionId(pub u64);

impl SessionId {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    pub fn marker(self) -> String {
        static START: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
        let stamp = START.get_or_init(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros()
        });
        format!("mcli-{}-{stamp}-{}", std::process::id(), self.0)
    }
}

pub struct Session {
    pub term: Arc<Mutex<Term<Listener>>>,
    pub listener: Listener,
    pub end: Arc<Mutex<Option<End>>>,
    pub id: SessionId,
    stop: Arc<OwnedHandle>,
    worker: Option<JoinHandle<()>>,
}

impl Session {
    #[cfg(test)]
    pub fn spawn(
        program: &Path,
        args: &[String],
        cwd: &Path,
        size: Size,
        wake: Waker,
    ) -> io::Result<Self> {
        Self::spawn_for(SessionId::new(), program, args, cwd, size, wake)
    }
    #[cfg(test)]
    pub fn spawn_for(
        id: SessionId,
        program: &Path,
        args: &[String],
        cwd: &Path,
        size: Size,
        wake: Waker,
    ) -> io::Result<Self> {
        Self::spawn_with_proxy(id, program, args, cwd, size, wake, None, None)
    }
    pub fn spawn_with_proxy(
        id: SessionId,
        program: &Path,
        args: &[String],
        cwd: &Path,
        size: Size,
        wake: Waker,
        proxy_directory: Option<&Path>,
        proxy_catalog: Option<&Path>,
    ) -> io::Result<Self> {
        let stop = Arc::new(pty::stop_event()?);
        let child = pty::spawn(
            program,
            args,
            cwd,
            size.cols,
            size.lines,
            &id.marker(),
            proxy_directory,
            proxy_catalog,
        )?;
        let (input, rx) = mpsc::channel();
        let listener = Listener {
            input,
            restore: Default::default(),
            size: Arc::new(Mutex::new(size)),
            resize_settle: Default::default(),
            dirty: Arc::new(AtomicBool::new(false)),
            visible: Arc::new(AtomicBool::new(false)),
            sidebar_dirty: Arc::new(AtomicBool::new(false)),
            shell: Default::default(),
            last_title: Default::default(),
            wake,
        };
        let config = Config {
            scrolling_history: 3000,
            default_cursor_style: CursorStyle {
                shape: CursorShape::Beam,
                ..CursorStyle::default()
            },
            ..Config::default()
        };
        let term = Arc::new(Mutex::new(Term::new(config, &size, listener.clone())));
        let console = Arc::new(Mutex::new(Some(child.console)));
        let end = Arc::new(Mutex::new(None));
        let read_term = term.clone();
        let read_events = listener.clone();
        let mut output = child.output;
        let reader = thread::spawn(move || {
            let mut parser = Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();
            let mut sgr = ConptySgr::default();
            let mut reports = crate::shell::ShellReports::default();
            let mut agy_stream = None;
            let mut agy_run = None;
            let mut foreground = None;
            let mut screen = None;
            let mut composer_busy = false;
            let mut composer_idle_sent = false;
            let mut osc7501_active = false;
            let mut osc7501_root: &'static str = "";
            let mut osc7501_children: std::collections::HashMap<String, &'static str> =
                std::collections::HashMap::new();
            let mut ready = false;
            let effective_osc7501 =
                |root: &'static str, children: &std::collections::HashMap<String, &'static str>| {
                    if root == "等待用户" || children.values().any(|v| *v == "等待用户") {
                        "等待用户"
                    } else if root == "运行中" || children.values().any(|v| *v == "运行中") {
                        "运行中"
                    } else {
                        root
                    }
                };
            let update_screen = |term: &Term<Listener>,
                                 agent,
                                 last: &mut Option<&'static str>,
                                 run: Option<&str>,
                                 was_busy: &mut bool,
                                 idle_sent: &mut bool,
                                 osc7501: bool,
                                 tui_seen: &mut bool| {
                let value = screen_state(term, agent);
                if *last != value {
                    *last = value;
                    read_events.shell.lock().unwrap().screen_state = Some(value);
                    read_events.notify_sidebar();
                }
                if !*tui_seen && (value.is_some() || tui_ready(term, agent)) {
                    *tui_seen = true;
                    read_events.shell.lock().unwrap().tui_ready = true;
                    read_events.notify_sidebar();
                }
                if value.is_some() {
                    *was_busy = false;
                    *idle_sent = false;
                } else if !osc7501
                    && let Some(run) = run
                    && let Some(busy) = agy_composer_busy(term)
                {
                    if *was_busy && !busy {
                        read_events
                            .shell
                            .lock()
                            .unwrap()
                            .agy_event(run, crate::shell::AgyEvent::Status("就绪"));
                        read_events.notify_sidebar();
                        *idle_sent = true;
                    } else if busy {
                        *idle_sent = false;
                    } else if !*idle_sent {
                        read_events
                            .shell
                            .lock()
                            .unwrap()
                            .agy_event(run, crate::shell::AgyEvent::ComposerIdle);
                        read_events.notify_sidebar();
                        *idle_sent = true;
                    }
                    *was_busy = busy;
                }
            };
            const FRAME_COALESCE: std::time::Duration = std::time::Duration::from_millis(16);
            const BG_SCREEN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
            let marker = id.marker();
            let mut bytes = [0u8; 32768];
            let mut last_wake = std::time::Instant::now() - FRAME_COALESCE;
            let mut pending_refresh = false;
            let mut last_bg_screen = std::time::Instant::now() - BG_SCREEN_INTERVAL;
            let mut bg_screen_dirty = false;
            loop {
                let settle_due = read_events
                    .resize_settle
                    .lock()
                    .unwrap()
                    .map(|s| s.quiet_until.unwrap_or(s.deadline));
                // Only wake on a timer while the CLI holds a synchronized update open.
                // A lost end sequence must not hide the last frame until another byte arrives.
                if let Some(deadline) = parser.sync_timeout().sync_timeout() {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        let mut term = read_term.lock().unwrap();
                        parser.stop_sync(&mut *term);
                        if let Some(agent) = foreground {
                            update_screen(
                                &term,
                                agent,
                                &mut screen,
                                agy_run.as_deref(),
                                &mut composer_busy,
                                &mut composer_idle_sent,
                                osc7501_active,
                                &mut ready,
                            );
                        }
                        read_events.refresh();
                        pending_refresh = false;
                        bg_screen_dirty = false;
                        let now = std::time::Instant::now();
                        last_wake = now;
                        last_bg_screen = now;
                    } else if matches!(pty::readable(&output), Ok(false)) {
                        thread::sleep(left.min(std::time::Duration::from_millis(4)));
                        continue;
                    }
                } else if let Some(settle_due) = settle_due {
                    let now = std::time::Instant::now();
                    if now >= settle_due {
                        *read_events.resize_settle.lock().unwrap() = None;
                        if let Some(agent) = foreground {
                            let term = read_term.lock().unwrap();
                            update_screen(
                                &term,
                                agent,
                                &mut screen,
                                agy_run.as_deref(),
                                &mut composer_busy,
                                &mut composer_idle_sent,
                                osc7501_active,
                                &mut ready,
                            );
                        }
                        read_events.refresh();
                        pending_refresh = false;
                        bg_screen_dirty = false;
                        last_wake = now;
                        last_bg_screen = now;
                    } else if matches!(pty::readable(&output), Ok(false)) {
                        thread::sleep((settle_due - now).min(std::time::Duration::from_millis(2)));
                        continue;
                    }
                } else if pending_refresh {
                    let now = std::time::Instant::now();
                    let due = last_wake + FRAME_COALESCE;
                    if now >= due {
                        if let Some(agent) = foreground {
                            let term = read_term.lock().unwrap();
                            update_screen(
                                &term,
                                agent,
                                &mut screen,
                                agy_run.as_deref(),
                                &mut composer_busy,
                                &mut composer_idle_sent,
                                osc7501_active,
                                &mut ready,
                            );
                        }
                        read_events.refresh();
                        pending_refresh = false;
                        bg_screen_dirty = false;
                        last_wake = now;
                        last_bg_screen = now;
                    } else if matches!(pty::readable(&output), Ok(false)) {
                        thread::sleep((due - now).min(std::time::Duration::from_millis(2)));
                        continue;
                    }
                } else if bg_screen_dirty {
                    let now = std::time::Instant::now();
                    let due = last_bg_screen + BG_SCREEN_INTERVAL;
                    if now >= due {
                        if let Some(agent) = foreground {
                            let term = read_term.lock().unwrap();
                            update_screen(
                                &term,
                                agent,
                                &mut screen,
                                agy_run.as_deref(),
                                &mut composer_busy,
                                &mut composer_idle_sent,
                                osc7501_active,
                                &mut ready,
                            );
                        }
                        bg_screen_dirty = false;
                        last_bg_screen = now;
                    } else if matches!(pty::readable(&output), Ok(false)) {
                        thread::sleep((due - now).min(std::time::Duration::from_millis(20)));
                        continue;
                    }
                }
                let mut eof = false;
                let mut chunks = 0usize;
                loop {
                    match output.read(&mut bytes) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(n) => {
                            reports.feed(&bytes[..n], &marker, |report| {
                                if let crate::shell::Report::PrepareAgy(run) = &report {
                                    let target = read_events.clone();
                                    let launch = run.clone();
                                    agy_stream = crate::hook_pipe::Server::agy_stream(
                                        &marker,
                                        run,
                                        move |id| {
                                            target.shell.lock().unwrap().agy_event(
                                                &launch,
                                                crate::shell::AgyEvent::Thread(id),
                                            );
                                            target.notify_sidebar();
                                        },
                                    )
                                    .ok();
                                    return;
                                }
                                if matches!(report, crate::shell::Report::Foreground(None)) {
                                    agy_stream.take();
                                }
                                if let crate::shell::Report::StatusQuery(bell) = report {
                                    read_events.reply(
                                        if bell {
                                            "\x1b]7501;?\x07"
                                        } else {
                                            "\x1b]7501;?\x1b\\"
                                        }
                                        .into(),
                                    );
                                    return;
                                }
                                let mut shell = read_events.shell.lock().unwrap();
                                match report {
                                    crate::shell::Report::Directory(path) => {
                                        shell.directory = Some(path)
                                    }
                                    crate::shell::Report::Foreground(agent) => {
                                        foreground = agent;
                                        agy_run = None;
                                        composer_busy = false;
                                        composer_idle_sent = false;
                                        osc7501_active = false;
                                        osc7501_root = "";
                                        osc7501_children.clear();
                                        ready = false;
                                        screen = None;
                                        *read_events.last_title.lock().unwrap() = None;
                                        shell.screen_state = Some(None);
                                        shell.status = None;
                                        shell.foreground = Some(agent);
                                        shell.launch = None;
                                        shell.explicit_id = None;
                                        shell.tui_ready = false;
                                    }
                                    crate::shell::Report::Launch(agent, run, explicit_id) => {
                                        if let Some(restore) =
                                            &mut *read_events.restore.lock().unwrap()
                                        {
                                            restore.bound = false;
                                        }
                                        foreground = Some(agent);
                                        composer_busy = false;
                                        composer_idle_sent = false;
                                        osc7501_active = false;
                                        osc7501_root = "";
                                        osc7501_children.clear();
                                        ready = false;
                                        agy_run = (agent == crate::shell::Foreground::Agy)
                                            .then(|| run.clone());
                                        if agent == crate::shell::Foreground::Agy {
                                            let target = read_events.clone();
                                            let launch = run.clone();
                                            agy_stream = crate::hook_pipe::Server::agy_stream(
                                                &marker,
                                                &run,
                                                move |id| {
                                                    target.shell.lock().unwrap().agy_event(
                                                        &launch,
                                                        crate::shell::AgyEvent::Thread(id),
                                                    );
                                                    target.notify_sidebar();
                                                },
                                            )
                                            .ok();
                                        } else {
                                            agy_stream.take();
                                        }
                                        screen = None;
                                        *read_events.last_title.lock().unwrap() = None;
                                        shell.screen_state = Some(None);
                                        shell.status = None;
                                        shell.foreground = Some(Some(agent));
                                        shell.launch = Some(run);
                                        shell.explicit_id = explicit_id;
                                        shell.tui_ready = false;
                                    }
                                    crate::shell::Report::Status(
                                        app,
                                        value,
                                        crate::shell::StatusSource::Osc7501,
                                    ) => {
                                        if let Some(agent) = app.or(foreground) {
                                            if foreground == Some(agent) {
                                                osc7501_active = true;
                                                osc7501_root = value;
                                                if value.is_empty() {
                                                    osc7501_children.clear();
                                                }
                                                if !ready && !value.is_empty() {
                                                    ready = true;
                                                    shell.tui_ready = true;
                                                }
                                            }
                                            let eff = if foreground == Some(agent) {
                                                effective_osc7501(osc7501_root, &osc7501_children)
                                            } else {
                                                value
                                            };
                                            if agent == crate::shell::Foreground::Agy {
                                                if let Some(run) = &agy_run {
                                                    shell.agy_event(
                                                        run,
                                                        crate::shell::AgyEvent::ProgramStatus(eff),
                                                    );
                                                } else {
                                                    shell.status = Some((agent, eff));
                                                }
                                            } else {
                                                shell.status = Some((agent, eff));
                                            }
                                        }
                                    }
                                    crate::shell::Report::ChildStatus(app, child_id, value) => {
                                        if let Some(agent) = app.or(foreground)
                                            && foreground == Some(agent)
                                        {
                                            osc7501_active = true;
                                            if value.is_empty() {
                                                osc7501_children.remove(&child_id);
                                            } else {
                                                osc7501_children.insert(child_id, value);
                                            }
                                            let eff =
                                                effective_osc7501(osc7501_root, &osc7501_children);
                                            if agent == crate::shell::Foreground::Agy {
                                                if let Some(run) = &agy_run {
                                                    shell.agy_event(
                                                        run,
                                                        crate::shell::AgyEvent::ProgramStatus(eff),
                                                    );
                                                } else {
                                                    shell.status = Some((agent, eff));
                                                }
                                            } else {
                                                shell.status = Some((agent, eff));
                                            }
                                        }
                                    }
                                    crate::shell::Report::Status(
                                        Some(crate::shell::Foreground::Agy),
                                        value,
                                        crate::shell::StatusSource::Osc9Ready,
                                    ) => {
                                        if !osc7501_active {
                                            if let Some(run) = &agy_run {
                                                shell.agy_event(
                                                    run,
                                                    crate::shell::AgyEvent::Status(value),
                                                );
                                            } else if foreground
                                                == Some(crate::shell::Foreground::Agy)
                                            {
                                                shell.status =
                                                    Some((crate::shell::Foreground::Agy, value));
                                            }
                                        }
                                    }
                                    crate::shell::Report::Status(
                                        _,
                                        _,
                                        crate::shell::StatusSource::Osc9Ready,
                                    ) => (),
                                    crate::shell::Report::RestoreStarted => {
                                        *read_events.restore.lock().unwrap() =
                                            RestoreSignals::open(&marker).ok();
                                    }
                                    crate::shell::Report::RestoreEnded(end) => {
                                        read_events.restore.lock().unwrap().take();
                                        shell.restore_end = Some(end);
                                    }
                                    crate::shell::Report::Proxy(summary) => {
                                        shell.proxy_summary = Some(summary);
                                    }
                                    crate::shell::Report::PrepareAgy(_)
                                    | crate::shell::Report::StatusQuery(_) => unreachable!(),
                                }
                                drop(shell);
                                read_events.notify_sidebar();
                            });
                            {
                                let mut term = read_term.lock().unwrap();
                                sgr.feed(&bytes[..n], |part| parser.advance(&mut *term, part));
                            }
                            chunks += 1;
                            if chunks >= 16 || !matches!(pty::readable(&output), Ok(true)) {
                                break;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            eof = true;
                            break;
                        }
                    }
                }
                if chunks > 0 && parser.sync_timeout().sync_timeout().is_none() {
                    let visible = read_events.visible.load(Ordering::Acquire);
                    let now = std::time::Instant::now();
                    let settling = {
                        let mut guard = read_events.resize_settle.lock().unwrap();
                        if let Some(s) = guard.as_mut() {
                            if now < s.deadline {
                                s.quiet_until = Some((now + RESIZE_QUIET_GAP).min(s.deadline));
                                true
                            } else {
                                *guard = None;
                                false
                            }
                        } else {
                            false
                        }
                    };
                    if visible {
                        if settling {
                            // Hold ConPTY's post-resize clear + redraw burst until the pipe goes
                            // briefly quiet or the short cap expires so partial frames never flash.
                            pending_refresh = true;
                        } else if now.saturating_duration_since(last_wake) >= FRAME_COALESCE {
                            if let Some(agent) = foreground {
                                let term = read_term.lock().unwrap();
                                update_screen(
                                    &term,
                                    agent,
                                    &mut screen,
                                    agy_run.as_deref(),
                                    &mut composer_busy,
                                    &mut composer_idle_sent,
                                    osc7501_active,
                                    &mut ready,
                                );
                            }
                            read_events.refresh();
                            pending_refresh = false;
                            bg_screen_dirty = false;
                            last_wake = now;
                            last_bg_screen = now;
                        } else {
                            // Coalesce high-frequency TUI bursts into at most one UI wake per 16 ms frame.
                            pending_refresh = true;
                        }
                    } else {
                        read_events.dirty.store(true, Ordering::Release);
                        if let Some(agent) = foreground {
                            if now.saturating_duration_since(last_bg_screen) >= BG_SCREEN_INTERVAL {
                                let term = read_term.lock().unwrap();
                                update_screen(
                                    &term,
                                    agent,
                                    &mut screen,
                                    agy_run.as_deref(),
                                    &mut composer_busy,
                                    &mut composer_idle_sent,
                                    osc7501_active,
                                    &mut ready,
                                );
                                bg_screen_dirty = false;
                                last_bg_screen = now;
                            } else {
                                bg_screen_dirty = true;
                            }
                        }
                    }
                }
                if eof {
                    break;
                }
            }
            {
                let mut term = read_term.lock().unwrap();
                parser.stop_sync(&mut *term);
                if let Some(agent) = foreground {
                    update_screen(
                        &term,
                        agent,
                        &mut screen,
                        agy_run.as_deref(),
                        &mut composer_busy,
                        &mut composer_idle_sent,
                        osc7501_active,
                        &mut ready,
                    );
                }
            }
            read_events.refresh();
        });
        let mut pipe = child.input;
        let write_console = console.clone();
        let writer = thread::spawn(move || {
            let mut pending = None;
            while let Some(item) = pending.take().or_else(|| rx.recv().ok()) {
                match item {
                    Input::Data(data) => {
                        if pipe.write_all(&data).is_err() {
                            break;
                        }
                    }
                    Input::Resize(mut target) => {
                        // Coalesce consecutive resizes while preserving order before any later input.
                        while let Ok(next) = rx.try_recv() {
                            match next {
                                Input::Resize(newer) => target = newer,
                                other => {
                                    pending = Some(other);
                                    break;
                                }
                            }
                        }
                        let guard = write_console.lock().unwrap();
                        if let Some(c) = guard.as_ref() {
                            let _ = c.resize(target.cols, target.lines);
                        }
                    }
                    Input::Stop => break,
                }
            }
        });
        let (stop_wait, closing, ended, events) =
            (stop.clone(), console.clone(), end.clone(), listener.clone());
        let worker = thread::spawn(move || {
            let mut status = match pty::wait(&child.process, &stop_wait) {
                Ok(Some(code)) => End::Code(code),
                Ok(None) => End::Closed,
                Err(e) => End::Error(e.to_string()),
            };
            let _ = events.input.send(Input::Stop);
            // Keep draining on the reader thread while ClosePseudoConsole flushes its last frame.
            let console = closing.lock().unwrap().take();
            if status == End::Closed {
                // On older Windows the console close itself can block on the client.
                let result = thread::scope(|scope| {
                    scope.spawn(move || drop(console));
                    pty::finish_requested_close(&child.process, 5000)
                });
                if let Err(e) = result {
                    status = End::Error(e.to_string());
                }
            } else {
                drop(console);
            }
            let _ = reader.join();
            let _ = writer.join();
            *ended.lock().unwrap() = Some(status);
            events.notify_sidebar();
            events.refresh();
        });
        Ok(Self {
            term,
            listener,
            end,
            id,
            stop,
            worker: Some(worker),
        })
    }
    pub fn key_input(&self, event: &rune::KeyEvent, bytes: Vec<u8>) {
        if (event.mods.ctrl || event.mods.cmd)
            && !event.mods.alt
            && (event.is_command('c') || event.is_command('d'))
        {
            self.claim_restore();
        } else if event.key == rune::Key::Enter
            && event.mods == rune::Mods::default()
            && let Some(s) = &*self.listener.restore.lock().unwrap()
        {
            // Enter can dismiss a setup prompt normally before an identity is available.
            // A subsequent connection failure still retries until the user takes over.
            pty::signal_event(if s.bound { &s.claimed } else { &s.clean_exit });
        }
        self.input(bytes);
    }
    pub fn input(&self, data: impl Into<Vec<u8>>) {
        let data = data.into();
        if data.is_empty() {
            return;
        }
        let mut term = self.term.lock().unwrap();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
            self.listener.refresh();
        }
        drop(term);
        let _ = self.listener.input.send(Input::Data(data));
    }
    pub fn focus_event(&self, focused: bool) {
        if self
            .mode()
            .contains(alacritty_terminal::term::TermMode::FOCUS_IN_OUT)
        {
            let seq = if focused { b"\x1b[I" } else { b"\x1b[O" };
            let _ = self.listener.input.send(Input::Data(seq.to_vec()));
        }
    }
    pub fn mode(&self) -> TermMode {
        *self.term.lock().unwrap().mode()
    }
    pub fn resize(&self, s: Size) -> io::Result<()> {
        let mut current = self.listener.size.lock().unwrap();
        if *current == s {
            return Ok(());
        }
        // Resize the in-memory grid immediately, coalesce ConPTY's clear+redraw burst,
        // and run ResizePseudoConsole off the UI thread.
        self.term.lock().unwrap().resize(s);
        *current = s;
        drop(current);
        let now = std::time::Instant::now();
        *self.listener.resize_settle.lock().unwrap() = Some(ResizeSettle {
            deadline: now + RESIZE_MAX_WAIT,
            quiet_until: None,
        });
        let _ = self.listener.input.send(Input::Resize(s));
        Ok(())
    }
    pub fn select_quiet(&self, col: u16, row: u16, right: bool, start: Option<SelectionType>) {
        let mut t = self.term.lock().unwrap();
        let p = Point::new(
            Line(i32::from(row) - t.grid().display_offset() as i32),
            Column(col as usize),
        );
        let side = if right { Side::Right } else { Side::Left };
        if let Some(kind) = start {
            t.selection = Some(Selection::new(kind, p, side));
        } else if let Some(s) = &mut t.selection {
            s.update(p, side);
        }
    }
    #[cfg(test)]
    pub fn select(&self, col: u16, row: u16, right: bool, start: Option<SelectionType>) {
        self.select_quiet(col, row, right, start);
        self.listener.refresh();
    }
    pub fn selected(&self) -> Option<String> {
        self.term
            .lock()
            .unwrap()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }
    /// Extract plain text from the bottom `max_lines` of non-empty terminal content (no ANSI escapes).
    pub fn screen_snapshot(&self, max_lines: usize) -> String {
        use alacritty_terminal::term::cell::Flags;
        let term = self.term.lock().unwrap();
        let grid = term.grid();
        let screen_lines = grid.screen_lines() as i32;
        let history_lines = grid.history_size() as i32;
        let cols = grid.columns();
        let want = max_lines.clamp(5, 80);
        // Always include the entire visible viewport plus up to `want` lines of scrollback,
        // then trim trailing blank rows below the cursor before taking the bottom `want` lines.
        let top = -(history_lines.min(want as i32));
        let mut lines: Vec<String> = Vec::with_capacity((screen_lines - top).max(0) as usize);
        for row_idx in top..screen_lines {
            let row = &grid[Line(row_idx)];
            let mut buf = String::with_capacity(cols);
            for col_idx in 0..cols {
                let cell = &row[Column(col_idx)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                buf.push(cell.c);
                if let Some(extra) = cell.zerowidth() {
                    for &ch in extra {
                        buf.push(ch);
                    }
                }
            }
            lines.push(buf.trim_end().to_owned());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if lines.len() > want {
            lines.drain(..lines.len() - want);
        }
        while lines.first().is_some_and(String::is_empty) {
            lines.remove(0);
        }
        // Collapse consecutive blank lines to keep mobile screen snapshots concise.
        let mut compact = Vec::with_capacity(lines.len());
        let mut prev_blank = false;
        for line in lines {
            let blank = line.is_empty();
            if blank && prev_blank {
                continue;
            }
            prev_blank = blank;
            compact.push(line);
        }
        compact.join("\n")
    }
    pub fn identify_restore(&self) {
        if let Some(signals) = &mut *self.listener.restore.lock().unwrap() {
            signals.bound = true;
            pty::signal_event(&signals.clean_exit);
        }
    }
    pub fn claim_restore(&self) {
        if let Some(signals) = &*self.listener.restore.lock().unwrap() {
            pty::signal_event(&signals.claimed);
        }
    }
    pub fn scroll_quiet(&self, lines: i32) {
        self.term
            .lock()
            .unwrap()
            .scroll_display(Scroll::Delta(lines));
    }
    #[cfg(test)]
    pub fn scroll(&self, lines: i32) {
        self.scroll_quiet(lines);
        self.listener.refresh();
    }
    pub fn request_close(&self) {
        self.claim_restore();
        pty::signal_event(&self.stop);
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.request_close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
