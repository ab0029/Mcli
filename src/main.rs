#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod clawbot;
mod conversation_store;
mod desktop_info;
mod discovery;
mod history;
mod history_view;
mod hook_pipe;
mod hook_wire;
mod input;
mod proxy;
mod pty;
mod shell;
mod sidebar;
#[cfg(windows)]
mod single_instance;
mod sound;
mod terminal;
mod terminal_view;
#[cfg(test)]
mod tests;
mod transcript;
#[cfg(windows)]
mod tray;
mod window_frame;
mod workbench;
mod workspace;
mod workspace_file;

use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::CursorShape;
use rune::{AppSpec, Cx, Drag, El, Ime, Key, KeyEvent, Phase, Tx, col, el, grid, row, text, tok};
#[cfg(test)]
use std::sync::mpsc;
#[cfg(test)]
use terminal::{End, Session, SessionId, Size};
use terminal_view::{HEIGHT, font, viewport};
use workspace::Workspace;

impl Workspace {
    fn resize(&mut self, w: f32, h: f32, cell: f32, scale: f32, tx: &mut Tx) {
        let metrics_changed = self.cell != cell || self.scale != scale;
        self.force |= metrics_changed;
        self.cell = cell;
        self.scale = scale;
        let Some(size) = viewport(w, h, cell, scale) else {
            if !metrics_changed {
                tx.live_only();
            }
            return;
        };
        if self.layout.queue_split_size(size, self.size) {
            if !metrics_changed {
                tx.live_only();
            }
        } else if !metrics_changed
            && let Some(wait) = self.layout.queue_window_size(size, self.size, tx.clock())
        {
            tx.wake_after(wait);
            tx.live_only();
        } else if self.apply_grid_size(size, tx.clock(), tx) || metrics_changed {
            self.layout.clear_pending_window_size(tx.clock());
            if self.force && self.resize_fallback_due.is_none() {
                self.refresh_active_screen(true);
            }
            tx.instant();
        } else {
            tx.live_only();
        }
    }

    fn apply_grid_size(&mut self, size: terminal::Size, clock: f64, tx: &mut Tx) -> bool {
        if size == self.size {
            return false;
        }
        self.size = size;
        let defer_paint = if let Some(i) = self.active_index()
            && let Some(s) = &self.tabs[i].session
        {
            if let Err(e) = s.resize(size) {
                self.tabs[i].error = Some(format!("终端尺寸更新失败：{e}"));
                false
            } else {
                // Keep the previous complete frame briefly while ConPTY + PowerShell/TUI finish
                // their clear+redraw burst, then paint the settled frame in one step (no flash).
                self.tabs[i].ended.is_none() && !self.screen.rows.is_empty()
            }
        } else {
            false
        };
        self.force = true;
        if defer_paint {
            const RESIZE_FALLBACK: std::time::Duration = std::time::Duration::from_millis(75);
            self.resize_fallback_due = Some(clock + RESIZE_FALLBACK.as_secs_f64());
            tx.wake_after(RESIZE_FALLBACK);
        } else {
            self.resize_fallback_due = None;
            self.refresh_active_screen(true);
        }
        true
    }
    fn cell_at(&self, x: f32, y: f32) -> (u16, u16, bool) {
        let col = (x / self.cell).clamp(0., f32::from(self.size.cols) - 0.001);
        (
            col as u16,
            (y / HEIGHT).clamp(0., f32::from(self.size.lines - 1)) as u16,
            col.fract() >= 0.5,
        )
    }
    fn terminal_has_focus(&self, tx: &Tx) -> bool {
        if !self.window_active {
            return false;
        }
        let key = tx.focused().and_then(|key| key.rsplit('/').next());
        key == Some("terminal")
            || self.focus_handoff.is_some_and(|id| {
                self.active == Some(id)
                    && (key
                        .and_then(|k| k.strip_prefix("session-"))
                        .and_then(|n| n.parse::<u64>().ok())
                        == Some(id.0)
                        || self
                            .current()
                            .and_then(|t| t.bound.as_ref())
                            .is_some_and(|task| {
                                key == Some(format!("task-{}-{}", task.agent, task.id).as_str())
                            }))
            })
    }
    fn paste(&self, tx: &mut Tx) {
        let generation = self.focus_generation;
        let recipient = self.active;
        tx.read_clipboard(move |w: &mut Self, content, tx| {
            if w.terminal_has_focus(tx)
                && w.focus_generation == generation
                && w.active == recipient
                && w.writable()
                && let Some(s) = w.session()
            {
                s.input(input::clipboard(&content, s.mode()));
            }
        });
    }
    fn key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        if self.workbench_key(event, tx) {
            return true;
        }
        if !self.terminal_has_focus(tx) {
            return false;
        }
        let Some(s) = self.session() else {
            return false;
        };
        let ctrl = event.mods.ctrl || event.mods.cmd;
        if ctrl
            && !event.mods.alt
            && event.is_command('c')
            && let Some(text) = s.selected()
        {
            tx.copy(text);
            return true;
        }
        if !self.writable() {
            return false;
        }
        if (ctrl && !event.mods.alt && event.is_command('v'))
            || (event.key == Key::Insert && event.mods.shift)
        {
            self.paste(tx);
            return true;
        }
        if !self.preedit.is_empty() {
            return true;
        }
        let cleared = {
            let Some(s) = self.session() else {
                return false;
            };
            let bytes = input::key(event, s.mode());
            if bytes.is_empty() {
                return false;
            }
            let cleared = s.term.lock().unwrap().selection.take().is_some();
            s.key_input(event, bytes);
            cleared
        };
        if cleared {
            self.refresh_active_screen(false);
            tx.instant();
        } else {
            // The PTY reader wakes Rune when the child actually echoes the key.
            tx.live_only();
        }
        true
    }
    fn ime(&mut self, event: &Ime, tx: &mut Tx) -> bool {
        if !self.terminal_has_focus(tx) || !self.writable() {
            return false;
        }
        match event {
            Ime::Preedit(text, ..) => {
                if !text.is_empty() {
                    self.composing = true;
                    self.discard_commit = false;
                }
                self.preedit.clone_from(text);
            }
            Ime::Commit(text) => {
                self.preedit.clear();
                self.composing = false;
                if !std::mem::take(&mut self.discard_commit)
                    && let Some(s) = self.session()
                {
                    s.input(text.as_bytes());
                }
            }
            Ime::Disabled => {
                self.preedit.clear();
                self.composing = false;
            }
            _ => return false,
        }
        tx.instant();
        true
    }
    fn drag(&mut self, d: &mut Drag, tx: &mut Tx) {
        let Some(i) = self.active_index() else { return };
        let Some(mode) = self.tabs[i].session.as_ref().map(|s| s.mode()) else {
            return;
        };
        let (col, row, right) = self.cell_at(d.x, d.y);
        let report = mode.intersects(TermMode::MOUSE_MODE)
            && !tx.mods().shift
            && self.tabs[i].ended.is_none();
        if d.phase != Phase::Down && !self.mouse_down {
            tx.live_only();
            return;
        }
        if d.phase == Phase::Down {
            tx.focus("terminal", false);
            self.mouse_down = true;
        }
        if report {
            let moved = self.last_cell != Some((col, row));
            if d.phase != Phase::Move
                || moved && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
            {
                if let Some(s) = &self.tabs[i].session {
                    s.input(input::mouse(
                        if d.phase == Phase::Move { 32 } else { 0 },
                        col,
                        row,
                        d.phase == Phase::Up,
                        tx.mods(),
                        mode,
                    ));
                }
            }
            if d.phase == Phase::Down {
                tx.instant();
            } else {
                tx.live_only();
            }
        } else {
            let start = (d.phase == Phase::Down).then_some(match d.clicks {
                2 => SelectionType::Semantic,
                3.. => SelectionType::Lines,
                _ => SelectionType::Simple,
            });
            let same_pos = d.phase != Phase::Down && self.last_drag == Some((col, row, right));
            if same_pos {
                tx.live_only();
            } else {
                if let Some(s) = &self.tabs[i].session {
                    s.select_quiet(col, row, right, start);
                }
                let changed = self.refresh_active_screen(false);
                if d.phase == Phase::Down || changed {
                    tx.instant();
                } else {
                    tx.live_only();
                }
            }
        }
        self.last_cell = Some((col, row));
        self.last_drag = (d.phase != Phase::Up).then_some((col, row, right));
        if d.phase == Phase::Up {
            self.mouse_down = false;
        }
    }
    fn wheel(&mut self, e: rune::WheelEvent, tx: &mut Tx) -> bool {
        let Some(i) = self.active_index() else {
            return false;
        };
        let Some(mode) = self.tabs[i].session.as_ref().map(|s| s.mode()) else {
            return false;
        };
        if e.dy == 0. {
            return false;
        }
        self.wheel_remainder += e.dy / HEIGHT;
        let lines = if e.notch {
            self.wheel_remainder = 0.;
            e.dy.signum() as i32 * 3
        } else {
            self.wheel_remainder.trunc() as i32
        };
        if !e.notch {
            self.wheel_remainder -= lines as f32;
        }
        if lines == 0 {
            tx.live_only();
            return true;
        }
        let (col, row, _) = self.cell_at(e.x, e.y);
        let report = mode.intersects(TermMode::MOUSE_MODE) && !e.mods.shift;
        if self.tabs[i].ended.is_none()
            && (report
                || mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
                    && !e.mods.shift)
        {
            let seq = if report {
                input::mouse(
                    if lines > 0 { 64 } else { 65 },
                    col,
                    row,
                    false,
                    e.mods,
                    mode,
                )
            } else {
                format!(
                    "\x1b{}{}",
                    if mode.contains(TermMode::APP_CURSOR) {
                        "O"
                    } else {
                        "["
                    },
                    if lines > 0 { "A" } else { "B" }
                )
                .into_bytes()
            };
            if let Some(s) = &self.tabs[i].session {
                s.input(seq.repeat(lines.unsigned_abs().min(40) as usize));
            }
            tx.live_only();
        } else {
            if let Some(s) = &self.tabs[i].session {
                s.scroll_quiet(lines);
            }
            if self.refresh_active_screen(false) {
                tx.instant();
            } else {
                tx.live_only();
            }
        }
        true
    }
}

fn workspace_view(w: &Workspace, cx: &mut Cx<Workspace>) -> El {
    // Keep the entire workbench free of transition/wheel-glide latency.
    cx.reduced_motion(true);
    // Resolve the rail and terminal in the same layout pass, before ConPTY sees its grid.
    if let Some(viewport) = cx.layout_size("workbench") {
        w.layout.viewport.set(viewport);
    }
    let theme = tok::theme(true);
    cx.focus_ring(1.5, -1., sidebar::ink(0x87b7ff));
    let cell = rune::text::advance("0000000000", font()) / 10.;
    let scale = cx.env().scale;
    let tab = w.current();
    let sidebar = sidebar::view(w);
    let mut right = col()
        .key("pane")
        .grow(1.)
        .min_w(0.)
        .h_full()
        .padding(8.)
        .clip_soft(0.)
        .fill(w.screen.bg);
    let layout = move |w: &mut Workspace, [_, _, width, height]: [f32; 4], tx: &mut Tx| {
        w.resize(width, height, cell, scale, tx)
    };
    if w.session().is_some() {
        let mut surface = grid(w.size.cols, cell, HEIGHT, w.screen.rows.clone())
            .grid_overlays(w.screen.selection_fills.clone())
            .key("terminal")
            .label("终端")
            .w_full()
            .h_full()
            .focusable(true)
            .no_focus_ring()
            .on_layout_tx(layout)
            .on_key(Workspace::key)
            .on_drag(Workspace::drag)
            .on_wheel(Workspace::wheel)
            .on_pointer_move_local(|w: &mut Workspace, p, tx| {
                tx.live_only();
                let Some(s) = w.session() else {
                    return;
                };
                let mode = s.mode();
                let (col, row, _) = w.cell_at(p.x, p.y);
                if mode.contains(TermMode::MOUSE_MOTION)
                    && !tx.mods().shift
                    && !w.mouse_down
                    && w.last_cell != Some((col, row))
                {
                    s.input(input::mouse(35, col, row, false, tx.mods(), mode));
                    w.last_cell = Some((col, row));
                }
            })
            .on_secondary_press_local(|w: &mut Workspace, p, tx| {
                tx.focus("terminal", false);
                tx.instant();
                let Some(s) = w.session() else {
                    return;
                };
                let mode = s.mode();
                if mode.intersects(TermMode::MOUSE_MODE) && !tx.mods().shift {
                    let (c, r, _) = w.cell_at(p.x, p.y);
                    s.input(input::mouse(2, c, r, false, tx.mods(), mode));
                    s.input(input::mouse(2, c, r, true, tx.mods(), mode));
                } else if let Some(text) = s.selected() {
                    tx.copy(text);
                } else {
                    w.paste(tx);
                }
            });
        if let Some((c, r, shape)) = w.screen.caret {
            let (x, y) = (f32::from(c) * cell, f32::from(r) * HEIGHT);
            if shape != CursorShape::Hidden {
                surface = surface.grid_caret(c, r);
            }
            if w.focused && w.writable() {
                let mut anchor = el()
                    .absolute(x, y)
                    .w(cell)
                    .h(HEIGHT)
                    .transition(rune::Transition::none());
                if !w.layout.is_dragging() && !w.mouse_down {
                    anchor = anchor.ime_cursor();
                }
                if !w.preedit.is_empty() {
                    anchor = anchor
                        .w((rune::text::advance(&w.preedit, font()) + 4.).max(cell))
                        .fill(w.screen.bg)
                        .child(text(&w.preedit).font(font()).color(w.screen.cursor_color));
                } else if shape == CursorShape::Beam {
                    anchor = anchor.child(el().w(2.).h(HEIGHT).fill(w.screen.cursor_color));
                } else if shape == CursorShape::HollowBlock {
                    anchor = anchor.border(1., w.screen.cursor_color);
                } else if shape == CursorShape::Underline {
                    anchor = anchor.child(
                        el().absolute(0., HEIGHT - 2.)
                            .w(cell)
                            .h(2.)
                            .fill(w.screen.cursor_color),
                    );
                }
                surface = surface.child(anchor);
            }
        }
        right = right.child(surface);
    } else {
        let hint = match tab {
            Some(t) if t.error.is_some() => t.error.as_deref().unwrap_or_default(),
            None if !w.first_open => "点击「新建终端」打开 PowerShell",
            _ => "正在启动 PowerShell…",
        };
        right = right.child(
            col()
                .w_full()
                .h_full()
                .center()
                .on_layout_tx(layout)
                .child(text(hint).color(theme.text_secondary)),
        );
    }
    let mut terminal_column = col().grow(1.).min_w(0.).h_full();
    if w.layout.show_metrics_bar {
        terminal_column = terminal_column.child(desktop_info::bar(w));
    }
    terminal_column = terminal_column.child(right.w_full().min_h(0.));
    let body = row()
        .key("workbench")
        .w_full()
        .grow(1.)
        .min_h(0.)
        .fill(theme.bg)
        .child(sidebar)
        .child(workbench::separator())
        .child(terminal_column)
        .child(history_view::drawer(w));
    col()
        .w_full()
        .h_full()
        .instant_changes()
        .motion(rune::Spring::NONE)
        .hover_motion_within(rune::Anim::INSTANT, rune::Anim::INSTANT)
        // Rune delivers native activation to the app root, in order with keyboard and IME input.
        .on_window_focus(|w: &mut Workspace, active, tx| {
            tx.instant();
            w.window_active = active;
            if !active {
                w.cancel_split();
                w.layout.menu = None;
                w.layout.cancel_recording();
                w.invalidate_input();
            }
        })
        .child(window_frame::titlebar(w, cx))
        .child(body)
        .children(workbench::menu_overlay(w))
}

fn app() -> rune::App<Workspace> {
    let spec: AppSpec<Workspace> = AppSpec {
        setup: Some(|w, wake| {
            w.wake = wake;
            #[cfg(not(test))]
            {
                w.desktop.start(w.wake.clone());
                if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
                    let root = std::path::PathBuf::from(&dir)
                        .join(crate::single_instance::data_folder_name());
                    w.enable_restore(&root.join("workspaces"));
                    w.layout = workbench::Layout::new(rune::prefs::Prefs::at(
                        root.join("preferences.json"),
                    ));
                    w.desktop.set_metrics_enabled(w.layout.show_metrics_bar);
                    w.sync_tray();
                    w.init_clawbot(root);
                }
                w.start_history();
            }
        }),
        on_key: Some(Workspace::workbench_key),
        poll: Some(Workspace::poll),
        ..AppSpec::new("Mcli", (1040., 640.), Workspace::open, workspace_view)
    };
    let app = rune::App::new(spec)
        .titlebar(rune::Titlebar::new(window_frame::HEIGHT))
        .on_ime(Workspace::ime)
        .on_close(Workspace::on_close_window)
        .on_quit(Workspace::on_quit_app);
    #[cfg(all(windows, not(test)))]
    let app = app.on_window(window_frame::install_win10_frame_fix);
    app
}

fn main() {
    #[cfg(all(windows, not(test)))]
    let Some(single) = single_instance::SingleInstance::acquire() else {
        return;
    };
    #[cfg(windows)]
    rune::set_app_icon_resource(1);
    let app = app().keep_running();
    #[cfg(all(windows, not(test)))]
    let _single_guard = {
        let wake_handle = app.handle();
        single.listen(move || {
            wake_handle.open();
            single_instance::bring_current_process_window_to_front();
        })
    };
    #[cfg(all(windows, not(test)))]
    let app = {
        let handle = app.handle();
        app.init(move || {
            let mut w = Workspace::open();
            w.set_app_handle(handle);
            w
        })
    };
    app.run();
}
