use crate::conversation_store::{Entry, PAGE, Summary};
use crate::history::{Command, Service, Update};
use crate::transcript::Key;
#[cfg(not(test))]
use crate::transcript::Roots;
use crate::workspace::Workspace;
use rune::{El, ScrollSpec, Tx, col, el, row, text, tok};
use std::path::PathBuf;

#[derive(Default)]
pub struct Panel {
    pub service: Option<Service>,
    recent: Vec<Summary>,
    recent_collapsed: bool,
    archive: bool,
    project: Option<PathBuf>,
    projects: Vec<PathBuf>,
    history_rows: Vec<Summary>,
    history_offset: usize,
    archive_generation: u64,
    open: bool,
    search: rune::TextEdit,
    search_before: Vec<i64>,
    searching: bool,
    more_hits: bool,
    archive_focus: bool,
    scanning: bool,
    key: Option<Key>,
    entries: Vec<Entry>,
    count: usize,
    offset: usize,
    generation: u64,
    expanded: Option<(usize, String)>,
    requested: Option<usize>,
    pub error: String,
}
impl Workspace {
    #[cfg(not(test))]
    pub fn start_history(&mut self) {
        if let (Some(dir), Some(roots)) = (std::env::var_os("LOCALAPPDATA"), Roots::installed()) {
            self.history.service = Some(Service::start(
                PathBuf::from(dir).join("Mcli/conversations.sqlite3"),
                roots,
                self.wake.clone(),
            ));
        }
    }
    fn history_command(&self, command: Command) {
        if let Some(service) = &self.history.service {
            let _ = service.send.send(command);
        }
    }
    pub fn reset_drawer(&mut self) {
        self.history_command(Command::CancelSearch);
        let key = self.current().and_then(|t| t.bound.clone());
        self.history.key = key;
        self.history.entries.clear();
        self.history.count = self.current().map_or(0, |t| t.prompt_count);
        self.history.offset = 0;
        self.history.generation = self.history.generation.wrapping_add(1);
        self.history.expanded = None;
        self.history.requested = None;
        self.history.search = Default::default();
        self.history.search_before.clear();
        self.history.searching = false;
        self.history.more_hits = false;
        self.load_page();
    }
    pub fn search_composing(&self) -> bool {
        self.history.open && self.history.search.is_composing()
    }
    fn change_search(&mut self) {
        self.history.search_before.clear();
        self.history.offset = 0;
        self.search_page();
    }
    fn search_page(&mut self) {
        let p = &mut self.history;
        p.generation = p.generation.wrapping_add(1);
        p.expanded = None;
        p.requested = None;
        p.entries.clear();
        p.more_hits = false;
        p.searching = !p.search.text.trim().is_empty();
        if p.searching {
            let Some(key) = p.key.clone() else { return };
            let cmd = Command::Search(
                key,
                p.search.text.trim().into(),
                *p.search_before.last().unwrap_or(&i64::MAX),
                p.generation,
            );
            self.history_command(cmd);
        } else {
            self.history_command(Command::CancelSearch);
            self.load_page();
        }
    }
    pub fn focus_footprints(&self, tx: &mut Tx) {
        let target = if self.history.key.is_some() && self.history.count > 0 {
            "prompt-search"
        } else {
            "close-footprints"
        };
        tx.focus_when_shown(target, true);
    }
    pub fn toggle_footprints(&mut self, tx: &mut Tx) {
        tx.instant();
        self.history.open = !self.history.open;
        self.reset_drawer();
        if self.history.open {
            self.invalidate_input();
            self.focus_footprints(tx);
        } else {
            self.select(self.active, tx);
        }
    }
    pub fn toggle_archive(&mut self, tx: &mut Tx) {
        tx.instant();
        self.history.archive = !self.history.archive;
        self.history.project = None;
        self.history.history_offset = 0;
        self.history.history_rows.clear();
        self.history.projects.clear();
        self.history.archive_focus = self.history.archive;
        if self.history.archive {
            self.layout.settings = false;
            self.layout.cancel_recording();
            self.invalidate_input();
            tx.focus_when_shown("history-toggle", true);
            self.load_history();
            self.history_command(Command::RefreshProjects);
        } else {
            self.select(self.active, tx);
        }
    }
    fn load_page(&self) {
        if self.history.open
            && self.history.search.text.trim().is_empty()
            && let Some(k) = self.history.key.clone()
        {
            self.history_command(Command::Page(
                k,
                self.history.offset,
                self.history.generation,
            ));
        }
    }
    fn load_history(&mut self) {
        self.history.archive_generation = self.history.archive_generation.wrapping_add(1);
        self.history_command(Command::History(
            self.history.project.clone(),
            self.history.history_offset,
            self.history.archive_generation,
        ));
    }
    fn live_task(&self, key: &Key) -> Option<&crate::workspace::Tab> {
        self.current().into_iter().chain(&self.tabs).find(|tab| {
            !tab.closing
                && tab.ended.is_none()
                && tab.error.is_none()
                && tab.bound.as_ref().or(tab.expected.as_ref()) == Some(key)
        })
    }
    fn select_history(&mut self, task: &Summary, tx: &mut Tx) {
        if Key::new(&task.key.agent, &task.key.id).is_none() {
            self.history.error = "历史中的官方对话编号无效。".into();
            return;
        }
        self.history.error.clear();
        if let Some(tab) = self.live_task(&task.key) {
            self.select(Some(tab.id), tx);
        } else {
            self.open_shell(Some(task.project.clone()), Some(task.key.clone()), tx);
        }
    }
    pub fn poll_history(&mut self, tx: &mut Tx) -> bool {
        let updates = self
            .history
            .service
            .as_ref()
            .map(|s| s.receive.try_iter().take(128).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut changed = false;
        for u in updates {
            match u {
                Update::RestoredReply(marker, run, key) => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| {
                        t.id.marker() == marker
                            && t.run.as_ref() == Some(&run)
                            && t.expected.as_ref() == Some(&key)
                            && t.bound.as_ref() == Some(&key)
                            && !t.closing
                            && t.ended.is_none()
                    }) {
                        t.mark_reply(self.active == Some(t.id));
                        changed = true;
                    }
                }
                Update::Activity(marker, run, state) => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| {
                        t.id.marker() == marker && t.run.as_ref() == Some(&run) && !t.closing
                    }) {
                        changed |= t.meter != state;
                        t.meter = state;
                        t.observe_reply(Some(t.id) == self.active);
                    }
                }
                Update::Scanning(scanning) => {
                    self.history.scanning = scanning;
                    if self.history.archive && !scanning {
                        self.load_history();
                    }
                    changed = true;
                }
                Update::Recent(items) => {
                    changed |= self.history.archive || items != self.history.recent;
                    self.history.recent = items;
                    if self.history.archive {
                        self.load_history();
                    }
                }
                Update::Bound(
                    marker,
                    run,
                    Summary {
                        key,
                        phase,
                        preview,
                        count,
                        last_prompt_ms,
                        ..
                    },
                ) => {
                    let Some(tab) = self.tabs.iter_mut().find(|t| {
                        t.id.marker() == marker && t.run.as_ref() == Some(&run) && !t.closing
                    }) else {
                        continue;
                    };
                    if let Some(expected) = &tab.expected
                        && tab.bound.as_ref() != Some(&key)
                    {
                        if expected == &key {
                            if let Some(session) = &tab.session {
                                session.identify_restore();
                            }
                        } else {
                            if tab.bound.is_none() {
                                self.history.error =
                                    "官方 CLI 打开的 ID 与所选历史不同，请核对终端提示。".into();
                            }
                            tab.cancel_restore();
                            changed = true;
                        }
                    }
                    let new_count = tab.prompt_count != count;
                    if tab.bound.as_ref().is_some_and(|prior| prior != &key) {
                        tab.reset_reply();
                    }
                    let activity = tab.last_activity_ms.max(last_prompt_ms);
                    changed |= tab.last_activity_ms != activity;
                    tab.last_activity_ms = activity;
                    changed |= tab.bound.as_ref() != Some(&key)
                        || tab.phase != phase
                        || tab.preview != preview
                        || (self.history.open && Some(tab.id) == self.active && new_count);
                    tab.bound = Some(key);
                    tab.phase = phase;
                    tab.preview = preview;
                    tab.prompt_count = count;
                    tab.observe_reply(Some(tab.id) == self.active);
                    if Some(tab.id) == self.active && new_count {
                        self.history.count = count;
                        self.load_page();
                    }
                }
                Update::Page(key, off, generation, items, count)
                    if self.history.open
                        && self.history.key.as_ref() == Some(&key)
                        && self.history.generation == generation
                        && self.history.offset == off =>
                {
                    changed = true;
                    self.history.entries = items;
                    self.history.count = count;
                }
                Update::Text(key, number, generation, copy, result)
                    if self.history.open
                        && self.history.key.as_ref() == Some(&key)
                        && self.history.generation == generation =>
                {
                    match result {
                        Ok(t) if copy => tx.copy(t),
                        Ok(t) if self.history.requested == Some(number) => {
                            changed = true;
                            self.history.expanded = Some((number, t))
                        }
                        Err(e) => {
                            self.history.error = e;
                            changed = true;
                        }
                        _ => (),
                    }
                }
                Update::Search(key, generation, mut entries, finished, error)
                    if self.history.open
                        && self.history.key.as_ref() == Some(&key)
                        && generation == self.history.generation
                        && !self.history.search.text.trim().is_empty() =>
                {
                    self.history.searching = !finished;
                    self.history.more_hits = entries.len() > PAGE;
                    entries.truncate(PAGE);
                    self.history.entries = entries;
                    if let Some(e) = error {
                        self.history.error = format!("搜索原文失败 · {e}");
                    }
                    changed = true;
                }
                Update::History(p, off, generation, items, projects)
                    if self.history.archive
                        && self.history.archive_generation == generation
                        && self.history.project == p
                        && self.history.history_offset == off =>
                {
                    changed = true;
                    self.history.history_rows = items;
                    self.history.projects = projects;
                    if std::mem::take(&mut self.history.archive_focus)
                        && !self.history.projects.is_empty()
                        && tx.focused().is_some_and(|k| k.ends_with("history-toggle"))
                    {
                        tx.focus_when_shown("history-project-0", true);
                    }
                }
                Update::Unbound(marker, run) => {
                    if let Some(tab) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.id.marker() == marker && t.run.as_ref() == Some(&run))
                    {
                        changed = true;
                        if tab.bound.take().is_some() {
                            tab.cancel_restore();
                        }
                        tab.preview.clear();
                        tab.phase.clear();
                        tab.prompt_count = 0;
                        tab.meter = None;
                        tab.reset_reply();
                    }
                }
                Update::Error(e) => {
                    self.history.error = e;
                    changed = true;
                }
                _ => (),
            }
        }
        if self.history.key.as_ref() != self.current().and_then(|t| t.bound.as_ref()) {
            self.reset_drawer();
            return true;
        }
        changed
    }
}

fn control(
    key: impl Into<String>,
    label: impl Into<String>,
    action: impl Fn(&mut Workspace, &mut Tx) + 'static,
) -> El {
    colored_control(key, label, tok::theme(true).text, action)
}
fn colored_control(
    key: impl Into<String>,
    label: impl Into<String>,
    color: rune::Color,
    action: impl Fn(&mut Workspace, &mut Tx) + 'static,
) -> El {
    let label = label.into();
    row()
        .key(key.into())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .min_w(0.)
        .padding(4.)
        .child(text(label).truncate().font(tok::type_::LABEL).color(color))
        .on_press_tx(action)
}
fn task_row(w: &Workspace, task: &Summary) -> El {
    use crate::sidebar::{MUTED, TEXT, agent_icon, ink, ui_font};
    let live = w.live_task(&task.key);
    let is_live = live.is_some();
    let unread = live.is_some_and(|t| t.unread && Some(t.id) != w.active);
    let stamp = (w.layout.width(w.cell) >= 228.)
        .then(|| {
            live.and_then(|t| t.last_activity_ms)
                .or(task.last_prompt_ms)
                .and_then(crate::desktop_info::local_clock)
        })
        .flatten();
    rune::memo(
        format!("task-{}-{}", task.key.agent, task.key.id),
        (
            &task.preview,
            &task.project,
            is_live,
            unread,
            stamp.as_deref(),
        ),
        || {
            let item = task.clone();
            let action = if is_live { "切回" } else { "恢复" };
            let label = format!(
                "{} · {} · {action}{}",
                item.key.label(),
                item.preview,
                if unread { "，有新回复" } else { "" }
            );
            let mut line = row()
                .role("button")
                .label(&label)
                .help(format!("{label}\n{}", item.project.display()))
                .w_full()
                .min_w(0.)
                .h(25.)
                .px(3.)
                .gap(8.)
                .items_center()
                .focusable(true)
                .radius(4.)
                .hover(crate::sidebar::ink(0x202a37))
                .child(agent_icon(&item.key.agent));
            if unread {
                line = line.child(crate::sidebar::unread_dot());
            }
            line = line.child(
                text(&item.preview)
                    .font(ui_font(13., 400.))
                    .truncate()
                    .min_w(0.)
                    .grow(1.)
                    .color(ink(TEXT)),
            );
            if let Some(stamp) = &stamp {
                line = line.child(text(stamp).font(ui_font(13., 400.)).color(ink(MUTED)));
            }
            line.on_press_tx(move |w: &mut Workspace, tx| w.select_history(&item, tx))
                .on_key(Workspace::key)
        },
    )
}
pub fn recent_view(w: &Workspace) -> El {
    use crate::sidebar::{MUTED, TEXT, icons, ink, ui_font};
    use rune::icon;
    let panel = &w.history;
    let wide = w.layout.width(w.cell) >= 290.;
    let mut area = col()
        .key("recent-tasks")
        .w_full()
        .min_h(0.)
        .pl(if wide { 16. } else { 8. })
        .pr(8.)
        .pb(6.);
    let shown = panel.archive || !panel.recent_collapsed && !panel.recent.is_empty();
    if shown {
        let height = (w.layout.viewport.get()[1] * 0.28).clamp(78., 285.);
        area = area.h(if panel.archive {
            height
        } else {
            height.min(72. + 32. * panel.recent.len() as f32)
        });
    }
    let mut heading = row().w_full().h(40.).items_center().gap(5.);
    let expanded = panel.archive || !panel.recent_collapsed;
    let title = row()
        .key("recent-toggle")
        .role("button")
        .label("展开或收起最近任务")
        .help("展开或收起最近任务")
        .focusable(true)
        .items_center()
        .gap(6.)
        .h(30.)
        .child(
            icon(
                if expanded {
                    &icons::DOWN
                } else {
                    &icons::RIGHT
                },
                12.,
            )
            .color(ink(TEXT)),
        )
        .child(
            text(if panel.archive {
                "全部历史"
            } else {
                "最近任务"
            })
            .font(ui_font(14., 620.))
            .color(ink(TEXT)),
        );
    let mut title = title;
    if !panel.archive {
        title = title.child(
            row()
                .h(19.)
                .px(6.)
                .center()
                .radius(8.)
                .fill(ink(0x222d3c))
                .child(
                    text(panel.recent.len().to_string())
                        .font(ui_font(13., 550.))
                        .color(ink(TEXT)),
                ),
        );
    }
    let collapse = |w: &mut Workspace, tx: &mut Tx| {
        tx.instant();
        if w.history.archive {
            w.toggle_archive(tx);
        } else {
            w.history.recent_collapsed = !w.history.recent_collapsed;
        }
    };
    heading = heading
        .child(title.on_press_tx(collapse))
        .child(el().grow(1.));
    let history_label = if panel.archive {
        "收起历史"
    } else {
        "全部历史"
    };
    for (key, label, glyph, action, act) in [
        (
            "footprint-toggle",
            "提示词足迹",
            &icons::PROMPTS,
            crate::workbench::ShortcutAction::Footprints,
            Workspace::toggle_footprints as fn(&mut Workspace, &mut Tx),
        ),
        (
            "history-toggle",
            history_label,
            &icons::HISTORY,
            crate::workbench::ShortcutAction::Archive,
            Workspace::toggle_archive,
        ),
    ] {
        let help_keys = w.layout.shortcuts.get(action).help_keys();
        heading = heading.child(
            row()
                .key(key)
                .role("button")
                .label(label)
                .help(label)
                .help_keys(help_keys)
                .focusable(true)
                .size(22., 24.)
                .center()
                .radius(5.)
                .hover(ink(0x263246))
                .child(icon(glyph, 15.).color(ink(MUTED)))
                .on_press_tx(act),
        );
    }
    if wide {
        heading = heading.child(control(
            "recent-fold",
            if expanded { "收起 ⌃" } else { "展开 ⌄" },
            collapse,
        ));
    }
    area = area.child(heading);
    let mut content = col().w_full();
    if panel.archive {
        for (i, p) in panel.projects.iter().enumerate() {
            let dir = p.clone();
            let name = p.file_name().unwrap_or(p.as_os_str()).to_string_lossy();
            content = content.child(colored_control(
                format!("history-project-{i}"),
                format!(
                    "{} {name}",
                    if panel.project.as_ref() == Some(p) {
                        "▾"
                    } else {
                        "›"
                    }
                ),
                crate::sidebar::ink(crate::sidebar::PROJECT_GREEN),
                move |w, tx| {
                    tx.instant();
                    w.history.project = if w.history.project.as_ref() == Some(&dir) {
                        None
                    } else {
                        Some(dir.clone())
                    };
                    w.history.history_offset = 0;
                    w.history.history_rows.clear();
                    w.load_history();
                },
            ));
            if panel.project.as_ref() == Some(p) {
                for item in panel.history_rows.iter().take(PAGE) {
                    content = content.child(task_row(w, item));
                }
                if panel.history_offset > 0 {
                    content = content.child(control("history-prev", "上一页", |w, tx| {
                        tx.instant();
                        w.history.history_offset = w.history.history_offset.saturating_sub(PAGE);
                        w.load_history();
                    }));
                }
                if panel.history_rows.len() > PAGE {
                    content = content.child(control("history-next", "下一页", |w, tx| {
                        tx.instant();
                        w.history.history_offset += PAGE;
                        w.load_history();
                    }));
                }
            }
        }
    } else if !panel.recent_collapsed {
        for item in &panel.recent {
            content = content.child(task_row(w, item));
        }
    }
    if shown {
        area = area.child(
            col()
                .key("recent-scroll")
                .w_full()
                .grow(1.)
                .pl(if wide { 8. } else { 0. })
                .min_h(0.)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(content),
        );
    }
    if panel.scanning || (panel.archive && panel.projects.is_empty()) {
        area = area.child(
            text(if panel.scanning {
                "正在查找项目历史…"
            } else {
                "尚无已登记的项目对话"
            })
            .font(tok::type_::LABEL)
            .color(crate::sidebar::ink(0xa7b2c2)),
        );
    }
    if !panel.error.is_empty() {
        area = area.child(
            text(&panel.error)
                .wrap_anywhere()
                .font(tok::type_::SECTION)
                .color(tok::theme(true).text_secondary),
        );
    }
    area
}
pub fn drawer(w: &Workspace) -> El {
    let p = &w.history;
    if !p.open {
        return el().w(0.).h(0.);
    }
    let theme = tok::theme(true);
    let mut content = col().w_full().gap(5.);
    let filtering = !p.search.text.trim().is_empty();
    if p.key.is_none() {
        let hint = if w.current().is_some_and(|t| t.foreground.is_some()) {
            "正在等待官方对话 ID"
        } else {
            "在 PowerShell 中启动 Codex 或 agy 后，这里会显示提问。"
        };
        content = content.child(text(hint).wrap_anywhere().color(theme.text_secondary));
    } else if filtering && (p.searching || p.entries.is_empty()) {
        content = content.child(
            text(if p.searching {
                "正在搜索提问原文…"
            } else {
                "没有匹配的提问"
            })
            .color(theme.text_secondary),
        );
    } else if p.count == 0 {
        content = content.child(text("这段对话还没有提问").color(theme.text_secondary));
    }
    for entry in &p.entries {
        let number = entry.number;
        let expanded = p
            .expanded
            .as_ref()
            .filter(|(selected, _)| *selected == number)
            .map(|(_, body)| body.as_str());
        let item = rune::memo(
            format!("footprint-row-{number}"),
            (&entry.snippet, &entry.time, expanded),
            || {
                let mut item = col()
                    .w_full()
                    .gap(2.)
                    .child(control(
                        format!("prompt-{number}"),
                        format!("{number}  {}", entry.snippet),
                        move |w, tx| {
                            tx.instant();
                            if w.history.requested == Some(number) {
                                w.history.requested = None;
                                w.history.expanded = None;
                                return;
                            }
                            w.history.requested = Some(number);
                            w.history.expanded = None;
                            if let Some(k) = w.history.key.clone() {
                                w.history_command(Command::Text(
                                    k,
                                    number,
                                    w.history.generation,
                                    false,
                                ));
                            }
                        },
                    ))
                    .child(
                        row()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .child(
                                text(&entry.time)
                                    .font(tok::type_::SECTION)
                                    .color(theme.text_secondary),
                            )
                            .child(control(
                                format!("copy-prompt-{number}"),
                                "复制",
                                move |w, _| {
                                    if let Some(k) = w.history.key.clone() {
                                        w.history_command(Command::Text(
                                            k,
                                            number,
                                            w.history.generation,
                                            true,
                                        ));
                                    }
                                },
                            )),
                    );
                if let Some(body) = expanded {
                    item = item.child(text(body).wrap_anywhere().color(theme.text));
                }
                item
            },
        );
        content = content.child(item);
    }
    let mut pages = row().gap(6.);
    if filtering && !p.search_before.is_empty() {
        pages = pages.child(control("prompt-prev", "上一页", |w, tx| {
            tx.instant();
            w.history.search_before.pop();
            w.search_page();
        }));
    } else if !filtering && p.offset > 0 {
        pages = pages.child(control("prompt-prev", "上一页", |w, tx| {
            tx.instant();
            w.history.offset = w.history.offset.saturating_sub(PAGE);
            w.load_page();
        }));
    }
    if filtering && p.more_hits {
        pages = pages.child(control("prompt-next", "下一页", |w, tx| {
            tx.instant();
            if let Some(entry) = w.history.entries.last() {
                w.history.search_before.push(entry.number as i64);
                w.search_page();
            }
        }));
    } else if !filtering && p.offset + p.entries.len() < p.count {
        pages = pages.child(control("prompt-next", "下一页", |w, tx| {
            tx.instant();
            w.history.offset += PAGE;
            w.load_page();
        }));
    }
    let mut panel = col()
        .key("footprints")
        .w(crate::workbench::DRAWER.min((w.layout.viewport.get()[0] - 12.).max(1.)))
        .h_full()
        .min_h(0.)
        .padding(9.)
        .gap(5.)
        .fill(theme.card)
        .child(
            row()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    text(format!("提示词足迹 {}", p.count))
                        .font(tok::type_::TITLE)
                        .color(theme.text),
                )
                .child(
                    control("close-footprints", "收起", Workspace::toggle_footprints)
                        .help("收起提示词足迹")
                        .help_keys(["Esc"]),
                ),
        );
    if let Some(k) = &p.key {
        panel = panel
            .child(
                text(format!(
                    "{} · {} · …{}",
                    k.label(),
                    w.current()
                        .and_then(|t| t.directory.file_name())
                        .unwrap_or_default()
                        .to_string_lossy(),
                    &k.id[k.id.len().saturating_sub(8)..]
                ))
                .truncate()
                .help(&k.id)
                .font(tok::type_::SECTION)
                .color(theme.text_secondary),
            )
            .child(
                rune::text_input(&p.search, tok::type_::LABEL)
                    .key("prompt-search")
                    .label("搜索提问原文")
                    .w_full()
                    .h(30.)
                    .padding(5.)
                    .radius(4.)
                    .fill(crate::sidebar::ink(0x25303d))
                    .single_line()
                    .placeholder("搜索这段对话的提问", theme.text_secondary)
                    .on_edit(|w: &mut Workspace| &mut w.history.search)
                    .on_input_key(Workspace::workbench_key)
                    .on_input_change(|w: &mut Workspace, _| w.change_search()),
            );
    }
    let panel = panel
        .child(
            col()
                .grow(1.)
                .min_h(0.)
                .w_full()
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(content),
        )
        .child(pages);
    panel
        .border(1., crate::sidebar::ink(0x283546))
        .pin(None, Some(0.), Some(0.), Some(0.))
}
