use crate::shell::Foreground;
use crate::workbench::{ContextMenu, Filter};
use crate::workspace::{Tab, Workspace};
use rune::{Color, El, Icon, ScrollSpec, TextStyle, Tx, col, el, icon, row, text, tok};
pub(super) mod icons;
mod navigation;

pub(super) fn ink(value: u32) -> Color {
    crate::terminal_view::color(alacritty_terminal::vte::ansi::Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    })
}

pub const PROJECT_GREEN: u32 = 0x9dd799;
pub const PANEL: u32 = 0x111722;
pub const LINE: u32 = 0x253141;
pub const TEXT: u32 = 0xe8edf5;
pub const MUTED: u32 = 0xb5c0d1;
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn ui_font(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight).family(rune::Family::Named("Segoe UI, Microsoft YaHei UI"))
}

pub fn unread_dot() -> El {
    el().size(8., 8.).radius(4.).fill(ink(0xff5c61))
}

const CODEX: Icon = Icon {
    name: "openai-blossom",
    view: 400.,
    body: include_str!("assets/openai-blossom.svg"),
};
const TERMINAL: Icon = Icon {
    name: "mcli-terminal",
    view: 24.,
    body: r#"<rect x="2" y="4" width="20" height="16" rx="3" fill="none" stroke="currentColor" stroke-width="2"/><path fill="none" stroke="currentColor" stroke-width="1.8" d="m6 9 4 3-4 2m6 1h5"/>"#,
};

pub fn agent_icon(agent: &str) -> El {
    use rune::{Shape, canvas};
    use std::sync::LazyLock;
    static SHELL: LazyLock<Shape> = LazyLock::new(|| {
        Shape::new()
            .fill(
                [[3., 2.], [15.7, 2.], [13., 14.], [0.3, 14.]],
                ink(0x487fc8),
            )
            .stroke([[5., 5.], [8.4, 7.7], [4.2, 10.6]], 1.35, Color::WHITE)
            .stroke([[8.5, 10.5], [11.2, 10.5]], 1.1, Color::WHITE)
    });
    static AGY: LazyLock<Shape> = LazyLock::new(|| {
        Shape::new()
            .stroke([[1., 13.], [3., 12.3], [5.2, 7.8]], 2.4, ink(0x64b9ff))
            .stroke([[5.2, 7.8], [6.8, 4.2]], 2.4, ink(0x69cc9b))
            .stroke([[6.8, 4.2], [7.9, 2.8], [9.1, 4.1]], 2.4, ink(0xf3ac62))
            .stroke([[9.1, 4.1], [11., 8.1]], 2.4, ink(0xc483d9))
            .stroke([[11., 8.1], [13.2, 12.3], [15., 13.]], 2.4, ink(0x718bff))
    });
    match agent {
        "agy" => canvas(AGY.clone()).size(16., 16.),
        "PowerShell" => canvas(SHELL.clone()).size(16., 16.),
        "codex" => icon(&CODEX, 16.).color(Color::WHITE),
        "kimi" => icon(&icons::KIMI, 16.).color(ink(0x67e8f9)),
        "grok" => icon(&icons::GROK, 16.).color(ink(0xf1f5f9)),
        _ => icon(&TERMINAL, 16.).color(ink(0xa2aebc)),
    }
}

fn button(
    key: String,
    glyph: &str,
    label: &str,
    act: impl Fn(&mut Workspace, &mut Tx) + 'static,
) -> El {
    row()
        .key(key)
        .role("button")
        .label(label)
        .help(label)
        .focusable(true)
        .size(22., 22.)
        .radius(4.)
        .center()
        .hover(ink(0x344356))
        .child(text(glyph).font(ui_font(13., 580.)).color(ink(TEXT)))
        .on_press_tx(act)
}

fn badge(status: &str, frame: usize) -> El {
    let (glyph, color, background) = match status {
        "运行中" => (None, 0x7bc6ff, 0x103557),
        "就绪" => (Some(&icons::READY), 0x9ae89f, 0x193d2b),
        "等待用户" => (Some(&icons::WAIT), 0xffce77, 0x493620),
        "出错" | "异常退出" => (Some(&icons::ERROR), 0xff9b9e, 0x472831),
        "已打断" => (Some(&icons::PAUSE), 0xceb5ff, 0x352d49),
        "关闭中" | "已关闭" => (Some(&icons::EXIT), 0xb6c1cf, 0x27313e),
        s if s.starts_with("已退出") => (Some(&icons::EXIT), 0xb6c1cf, 0x27313e),
        _ => (Some(&icons::ONLINE), 0xaec8dc, 0x233141),
    };
    let symbol = match glyph {
        Some(glyph) => icon(glyph, 12.).color(ink(color)),
        None => text(SPINNER[frame])
            .font(TextStyle::new(13., 600.))
            .color(ink(color)),
    };
    row()
        .items_center()
        .h(19.)
        .px(6.)
        .gap(4.)
        .radius(8.)
        .fill(ink(background))
        .child(symbol)
        .child(text(status).font(ui_font(12., 550.)).color(ink(color)))
}

fn session_row(w: &Workspace, tab: &Tab) -> El {
    let id = tab.id;
    let name = tab.label();
    let agent = tab.foreground.map(Foreground::name).unwrap_or(&name);
    let state = tab.state_text();
    let selected = w.active == Some(id);
    let unread = !selected && tab.unread;
    let frame = if state == "运行中" { w.spinner } else { 0 };
    let width = w.layout.width(w.cell);
    let show_time = width >= 228.;
    let indent = if width >= 325. {
        47
    } else if width >= 230. {
        30
    } else {
        14
    };
    let unbound = tab.foreground.is_some() && tab.bound.is_none();
    let detail = tab.error.as_deref().unwrap_or(&tab.phase);
    let summary = if tab.preview.is_empty() {
        if unbound {
            "未关联官方对话"
        } else {
            tab.directory.to_str().unwrap_or("")
        }
    } else {
        &tab.preview
    };
    let retry = (tab.ended.is_some() || tab.error.is_some()) && !tab.closing;
    rune::memo(
        format!("session-row-{}", id.0),
        (
            &name,
            &state,
            unbound,
            detail,
            selected,
            summary,
            retry,
            tab.closing,
            (
                &tab.directory,
                unread,
                frame,
                (show_time, indent),
                tab.last_activity_ms,
            ),
        ),
        || {
            let mut help = format!("{name} · {state}");
            if unbound {
                help.push_str(" · 未关联");
            }
            if !detail.is_empty() && detail != state {
                help.push_str(&format!("\n{detail}"));
            }
            if !summary.is_empty() {
                help.push_str(&format!("\n{summary}"));
            }
            let mut first = row()
                .w_full()
                .items_center()
                .gap(8.)
                .child(
                    row()
                        .w(if indent == 47 { 22. } else { 17. })
                        .items_center()
                        .child(agent_icon(agent)),
                )
                .child(
                    text(&name)
                        .font(ui_font(15., 550.))
                        .min_w(0.)
                        .truncate()
                        .color(ink(0xe4e9f1)),
                );
            if unread {
                first = first.child(unread_dot());
            }
            let first = first.child(el().grow(1.)).child(badge(&state, frame));
            let mut second = row()
                .w_full()
                .items_center()
                .gap(7.)
                .pl(if indent == 47 { 30. } else { 25. })
                .child(
                    text(summary)
                        .font(ui_font(13., 400.))
                        .grow(1.)
                        .min_w(0.)
                        .truncate()
                        .color(ink(MUTED)),
                );
            if show_time
                && let Some(stamp) = tab
                    .last_activity_ms
                    .and_then(crate::desktop_info::local_clock)
            {
                second = second.child(text(stamp).font(ui_font(13., 400.)).color(ink(MUTED)));
            }
            if retry {
                second = second.child(el().w(39.));
            }
            let mut select = col()
                .key(format!("session-{}", id.0))
                .role("button")
                .label(format!(
                    "{name} · {}，{state}{}",
                    id.0,
                    if unread { "，有新回复" } else { "" }
                ))
                .help(help)
                .grow(1.)
                .min_w(0.)
                .h(if selected { 53. } else { 51. })
                .pl(indent as f32)
                .pr(13.)
                .justify_center()
                .gap(2.)
                .child(first)
                .child(second);
            if !tab.closing {
                select = select
                    .focusable(true)
                    .on_key(Workspace::key)
                    .on_press_tx(move |w: &mut Workspace, tx| w.select(Some(id), tx));
            }
            let line = row()
                .w_full()
                .items_center()
                .radius(6.)
                .hover_group()
                .hover_group_focus()
                .fill(if selected {
                    ink(0x1d2e47)
                } else {
                    Color::TRANSPARENT
                })
                .border(
                    1.,
                    if selected {
                        ink(0x3778b7)
                    } else {
                        Color::TRANSPARENT
                    },
                )
                .hover(ink(if selected { 0x223651 } else { 0x1a2534 }))
                .child(select);
            let mut actions = if retry {
                row().pin(None, None, Some(3.), Some(3.))
            } else {
                row().pin(Some(2.), None, None, Some(2.))
            };
            if retry {
                let path = tab.directory.clone();
                actions = actions.child(
                    button(
                        format!("retry-{}", id.0),
                        "↻",
                        "重开终端",
                        move |w, tx| {
                            w.close(id, tx);
                            w.new_shell((!path.as_os_str().is_empty()).then(|| path.clone()), tx);
                        },
                    )
                    .size(20., 20.),
                );
            }
            let actions = actions.child(
                button(
                    format!("close-{}", id.0),
                    "×",
                    "关闭终端",
                    move |w, tx| w.close(id, tx),
                )
                .size(20., 20.)
                .group_opacity(0., 1.),
            );
            line.child(actions)
        },
    )
}

pub(crate) fn normalize_explorer_path(dir: &std::path::Path) -> std::path::PathBuf {
    let raw = dir.to_string_lossy();
    let stripped = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        raw.into_owned()
    };
    let mut normalized = stripped.replace('/', r"\");
    // Keep drive root like `C:\`, strip redundant trailing backslashes elsewhere.
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    std::path::PathBuf::from(normalized)
}

pub(crate) fn open_in_explorer(w: &mut Workspace, dir: &std::path::Path) {
    let target = normalize_explorer_path(dir);
    if !target.is_dir() {
        w.history.error = format!("目录不存在：{}", target.display());
        return;
    }
    #[cfg(all(windows, not(test)))]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::{null, null_mut};
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let op: Vec<u16> = "open\0".encode_utf16().collect();
        let mut file: Vec<u16> = target.as_os_str().encode_wide().collect();
        file.push(0);
        let rc = unsafe {
            ShellExecuteW(
                null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if rc <= 32 {
            w.history.error = format!("无法打开资源管理器（错误码 {rc}）：{}", target.display());
        }
    }
    #[cfg(not(all(windows, not(test))))]
    let _ = w;
}

pub(super) fn view(w: &Workspace) -> El {
    let width = w.layout.width(w.cell);
    let rail = row().h_full().child(navigation::rail(w));
    let pane = col()
        .key("projects")
        .w(width)
        .h_full()
        .min_h(0.)
        .gradient(ink(0x121a25), ink(0x101621));
    if w.layout.settings {
        return rail.child(pane.child(navigation::settings(w)));
    }
    let query = w.layout.search.text.trim().to_lowercase();
    let searching = !query.is_empty();
    let mut content = col().w_full();
    let mut visible_groups = 0;
    for group in w.groups() {
        let first = group[0];
        let total = group.len();
        let visible: Vec<_> = group
            .into_iter()
            .filter(|t| {
                w.layout.filter.includes(t, w.active == Some(t.id))
                    && w.layout.matches_search(t, &query)
            })
            .collect();
        if visible.is_empty() {
            continue;
        }
        visible_groups += 1;
        let path = first.directory.clone();
        let project = first.project.clone();
        let menu_dir = path.clone();
        let menu_anchor = format!("project-{}", first.id.0);
        let open = !w.layout.collapsed_for_search(searching).contains(&project);
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let name = if name.is_empty() {
            "终端".into()
        } else {
            name
        };
        let waiting = visible
            .iter()
            .filter(|t| t.state_text() == "等待用户")
            .count();
        let path_hint = path.display().to_string();
        let mut title = row()
            .key(format!("project-{}", first.id.0))
            .role("button")
            .focusable(true)
            .label(format!(
                "{name}，{} 个会话，{path_hint}{}",
                total,
                if !open && visible.iter().any(|t| t.unread) {
                    "，有新回复"
                } else {
                    ""
                }
            ))
            .help(path_hint)
            .grow(1.)
            .min_w(0.)
            .h(30.)
            .items_center()
            .gap(9.)
            .child(icon(if open { &icons::DOWN } else { &icons::RIGHT }, 13.).color(ink(TEXT)))
            .child(icon(&icons::FOLDER, 16.).color(ink(0xd5dfeb)))
            .child(
                text(name.as_ref())
                    .font(ui_font(15., 550.))
                    .min_w(0.)
                    .truncate()
                    .color(ink(PROJECT_GREEN)),
            )
            .child(
                text(format!("({total})"))
                    .font(ui_font(14., 420.))
                    .color(ink(0x9baaba)),
            )
            .on_press_tx(move |w: &mut Workspace, tx| {
                tx.instant();
                let collapsed = if searching {
                    &mut w.layout.search_collapsed
                } else {
                    &mut w.layout.collapsed
                };
                if !collapsed.remove(&project) {
                    collapsed.insert(project.clone());
                }
            })
            .on_secondary_press_local(move |w: &mut Workspace, _, tx| {
                tx.instant();
                w.layout.menu = Some(ContextMenu::Project {
                    anchor: menu_anchor.clone(),
                    dir: menu_dir.clone(),
                    searching,
                });
            });
        if !open && visible.iter().any(|t| t.unread) {
            title = title.child(unread_dot());
        }
        if let Some(branch) = w.desktop.branches.get(&first.project) {
            title = title.child(
                text(branch)
                    .font(tok::type_::SECTION)
                    .min_w(0.)
                    .truncate()
                    .color(ink(0xd7aeb8))
                    .help(branch),
            );
        }
        if !open && waiting > 0 {
            title = title.child(
                text(format!("◆ {waiting}"))
                    .font(tok::type_::LABEL)
                    .help(format!("{waiting} 个会话等待用户"))
                    .color(ink(0xf2bf69)),
            );
        }
        let mut heading = row()
            .w_full()
            .pl(7.)
            .pr(10.)
            .hover_group()
            .hover_group_focus()
            .items_center()
            .child(title);
        if !path.as_os_str().is_empty() {
            heading = heading.child(
                button(
                    format!("project-new-{}", first.id.0),
                    "＋",
                    "在项目中新建终端",
                    move |w, tx| w.new_shell(Some(path.clone()), tx),
                )
                .group_opacity(0., 1.),
            );
        }
        let mut project = col()
            .key(format!("group-{}", first.id.0))
            .w_full()
            .gap(0.)
            .child(el().mx(5.).mr(12.).h(1.).fill(ink(LINE)))
            .child(heading);
        if open {
            for tab in visible {
                project = project.child(session_row(w, tab));
            }
        }
        content = content.child(project);
    }
    if visible_groups == 0 && (searching || w.layout.filter != Filter::All) {
        content = content.child(
            text("没有匹配的会话")
                .font(tok::type_::LABEL)
                .color(ink(MUTED))
                .padding(12.),
        );
    }
    rail.child(
        pane.child(navigation::header(w))
            .child(
                col()
                    .key("project-scroll")
                    .w_full()
                    .mt(10.)
                    .pl(if width < 289. { 4. } else { 11. })
                    .grow(1.)
                    .min_h(0.)
                    .scroll(ScrollSpec {
                        stick_bottom: false,
                    })
                    .child(content),
            )
            .child(
                el().key("recent-divider")
                    .mx(if width < 289. { 8. } else { 16. })
                    .h(1.)
                    .fill(ink(LINE)),
            )
            .child(crate::history_view::recent_view(w)),
    )
}
