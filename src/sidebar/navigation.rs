use super::{Filter, LINE, MUTED, TEXT, Workspace, button, icons, ink, ui_font};
use crate::workbench::ShortcutAction;
use rune::{Color, El, Icon, PopupItem, PopupMenu, ScrollSpec, col, el, icon, row, text};

fn destination(key: &str, label: &str, glyph: &'static Icon, settings: bool, w: &Workspace) -> El {
    let selected = settings == w.layout.settings;
    let color = ink(if selected { 0x77c9ff } else { 0xd9e2f0 });
    col()
        .key(key)
        .role("button")
        .label(label)
        .help(label)
        .focusable(true)
        .w((w.layout.rail() - 13.).min(54.))
        .h(64.)
        .center()
        .gap(5.)
        .radius(10.)
        .fill(if selected {
            ink(0x172b46)
        } else {
            Color::TRANSPARENT
        })
        .hover(ink(if selected { 0x1c3451 } else { 0x1b2635 }))
        .child(icon(glyph, 23.).color(color))
        .child(text(label).font(ui_font(13., 520.)).color(color))
        .on_press_tx(move |w: &mut Workspace, tx| {
            tx.instant();
            w.layout.settings = settings;
            if !settings {
                w.layout.cancel_recording();
                w.select(w.active, tx);
            }
        })
}

pub(super) fn rail(w: &Workspace) -> El {
    row()
        .key("navigation")
        .w(w.layout.rail())
        .h_full()
        .child(
            col()
                .grow(1.)
                .h_full()
                .items_center()
                .pt(16.)
                .pb(34.)
                .fill(ink(0x0e1520))
                .child(destination(
                    "nav-projects",
                    "项目",
                    &icons::FOLDER,
                    false,
                    w,
                ))
                .child(el().grow(1.))
                .child(destination(
                    "nav-settings",
                    "设置",
                    &icons::SETTINGS,
                    true,
                    w,
                )),
        )
        .child(el().key("nav-divider").w(1.).h_full().fill(ink(0x2b3748)))
}

pub(super) fn header(w: &Workspace) -> El {
    let width = w.layout.width(w.cell);
    let compact = width < 289.;
    let mut search = row()
        .key("sidebar-search-box")
        .w_full()
        .h(35.)
        .items_center()
        .gap(11.)
        .px(11.)
        .radius(7.)
        .border(1., ink(0x324054))
        .fill(ink(0x1b2330))
        .child(icon(&icons::SEARCH, 17.).color(ink(0x9eb0c5)))
        .child(
            rune::text_input(&w.layout.search, ui_font(13., 400.))
                .key("sidebar-search")
                .label("搜索项目或会话")
                .h(32.)
                .pt(7.)
                .min_w(0.)
                .grow(1.)
                .single_line()
                .color(ink(TEXT))
                .placeholder("搜索项目 / 会话…", ink(0xa5b2c4))
                .on_edit(|w: &mut Workspace| &mut w.layout.search)
                .on_input_change(|w: &mut Workspace, _| w.layout.search_collapsed.clear())
                .on_input_key(Workspace::workbench_key),
        );
    if !compact && w.layout.search.text.is_empty() {
        search = search.child(
            row()
                .px(5.)
                .h(20.)
                .center()
                .radius(5.)
                .fill(ink(0x2b3545))
                .child(
                    text(
                        w.layout
                            .shortcuts
                            .get(ShortcutAction::Search)
                            .compact_display(),
                    )
                    .font(ui_font(12., 460.))
                    .color(ink(TEXT)),
                ),
        );
    }
    let shortcuts = button(
        "project-menu".into(),
        "•••",
        "项目操作",
        |_, tx| {
            let menu = PopupMenu::new()
                .item(PopupItem::new("footprints", "提示词足迹"))
                .item(PopupItem::new("history", "全部历史"));
            tx.popup_menu(
                menu,
                "project-menu",
                |w: &mut Workspace, picked, tx| match picked {
                    Some("footprints") => w.toggle_footprints(tx),
                    Some("history") => w.toggle_archive(tx),
                    _ => (),
                },
            );
        },
    )
    .size(if compact { 28. } else { 44. }, 26.)
    .fill(ink(0x202936));
    let head = row()
        .w_full()
        .h(31.)
        .items_center()
        .gap(8.)
        .child(text("项目").font(ui_font(20., 660.)).color(ink(TEXT)))
        .child(el().grow(1.))
        .child(
            button(
                "new-shell".into(),
                if compact { "＋" } else { "＋ 新建" },
                "新建终端",
                |w, tx| {
                    let dir = w
                        .current()
                        .map(|t| t.directory.clone())
                        .filter(|p| !p.as_os_str().is_empty());
                    w.new_shell(dir, tx);
                },
            )
            .size(if compact { 29. } else { 73. }, 26.)
            .fill(ink(0x202936))
            .help_keys(w.layout.shortcuts.get(ShortcutAction::NewShell).help_keys()),
        )
        .child(shortcuts);
    let mut filters = row().w_full().items_center().justify_between();
    let dense = width < 242.;
    for (filter, key, label) in Filter::ITEMS {
        let count = w
            .tabs
            .iter()
            .filter(|t| filter.includes(t, w.active == Some(t.id)))
            .count();
        let selected = w.layout.filter == filter;
        let mut chip = row()
            .key(format!("filter-{key}"))
            .role("button")
            .label(format!("{label} {count}"))
            .help(format!("{label} · {count} 个会话"))
            .focusable(true)
            .h(28.)
            .px(if compact { 3. } else { 8. })
            .gap(if compact { 2. } else { 5. })
            .radius(10.)
            .items_center()
            .fill(if selected {
                ink(0x1e354f)
            } else {
                Color::TRANSPARENT
            })
            .border(
                1.,
                if selected {
                    ink(0x428bd4)
                } else {
                    Color::TRANSPARENT
                },
            )
            .hover(ink(0x263246))
            .on_press_tx(move |w: &mut Workspace, tx| {
                tx.instant();
                w.layout.filter = filter;
            })
            .child(
                text(label)
                    .font(ui_font(if compact { 12. } else { 13. }, 550.))
                    .color(ink(TEXT)),
            );
        if !dense {
            let mut number = row()
                .h(19.)
                .px(if compact { 4. } else { 6. })
                .center()
                .radius(9.)
                .fill(ink(if selected { 0x2c5892 } else { 0x2a3444 }))
                .child(
                    text(count.to_string())
                        .font(ui_font(13., 550.))
                        .color(ink(TEXT)),
                );
            if filter == Filter::Unread && count > 0 {
                number = number.child(super::unread_dot().size(6., 6.).pin(
                    None,
                    Some(-2.),
                    Some(-2.),
                    None,
                ));
            }
            chip = chip.child(number);
        }
        filters = filters.child(chip);
    }
    col()
        .w_full()
        .pl(if compact { 8. } else { 16. })
        .pr(if compact { 8. } else { 13. })
        .pt(10.)
        .gap(12.)
        .child(head)
        .child(search)
        .child(filters)
}

pub(super) fn settings(w: &Workspace) -> El {
    let toggle_card = |key: &'static str,
                       title: &'static str,
                       desc: &'static str,
                       on: bool,
                       act: fn(&mut Workspace, bool, &mut rune::Tx)| {
        let status = if on { "开启中" } else { "关闭中" };
        row()
            .key(key)
            .role("switch")
            .label(format!("{title}，{status}"))
            .focusable(true)
            .w_full()
            .padding(11.)
            .gap(10.)
            .items_center()
            .radius(8.)
            .fill(ink(0x16202e))
            .border(1., ink(if on { 0x3778b7 } else { 0x263244 }))
            .hover(ink(0x1c2838))
            .on_key(Workspace::workbench_key)
            .on_press_tx(move |w: &mut Workspace, tx| act(w, !on, tx))
            .child(
                col()
                    .grow(1.)
                    .min_w(0.)
                    .gap(3.)
                    .child(text(title).font(ui_font(14., 600.)).color(ink(TEXT)))
                    .child(
                        text(desc)
                            .font(ui_font(12., 400.))
                            .wrap_anywhere()
                            .color(ink(MUTED)),
                    ),
            )
            .child(
                row()
                    .h(24.)
                    .px(9.)
                    .center()
                    .radius(12.)
                    .fill(ink(if on { 0x2665a6 } else { 0x273242 }))
                    .child(text(status).font(ui_font(12., 580.)).color(ink(if on {
                        0xffffff
                    } else {
                        MUTED
                    }))),
            )
    };
    let tray_card = toggle_card(
        "setting-close-to-tray",
        "关闭时最小化到托盘",
        "点击右上角关闭按钮时隐藏到系统托盘，保持后台会话运行；右键托盘图标可退出。",
        w.layout.close_to_tray,
        Workspace::set_close_to_tray,
    );
    let metrics_card = toggle_card(
        "setting-metrics-bar",
        "顶部整机状态栏",
        "在终端上方显示整机 CPU、内存与网速监控；关闭可留出更多终端行数并停止后台采样。",
        w.layout.show_metrics_bar,
        Workspace::set_show_metrics_bar,
    );

    let customized = !w.layout.shortcuts.is_default();
    let mut shortcut_header = row()
        .w_full()
        .items_center()
        .justify_between()
        .child(text("快捷键").font(ui_font(14., 600.)).color(ink(TEXT)));
    if customized {
        shortcut_header = shortcut_header.child(
            row()
                .key("setting-shortcuts-reset")
                .role("button")
                .label("恢复默认快捷键")
                .focusable(true)
                .h(22.)
                .px(8.)
                .center()
                .radius(5.)
                .fill(ink(0x202b3a))
                .hover(ink(0x2b394d))
                .on_key(Workspace::workbench_key)
                .on_press_tx(|w: &mut Workspace, tx| w.reset_shortcuts(tx))
                .child(
                    text("恢复默认")
                        .font(ui_font(12., 520.))
                        .color(ink(0x8bc5ff)),
                ),
        );
    }

    let mut shortcut_list = col().w_full().gap(6.).child(shortcut_header);
    if let Some(notice) = &w.layout.shortcut_notice {
        shortcut_list = shortcut_list.child(
            text(notice)
                .font(ui_font(12., 450.))
                .wrap_anywhere()
                .color(ink(0xffce77)),
        );
    } else if w.layout.recording_shortcut.is_some() {
        shortcut_list = shortcut_list.child(
            text("请直接按下新的组合键（需含 Ctrl / Alt 或 F1-F12），按 Esc 取消。")
                .font(ui_font(12., 450.))
                .wrap_anywhere()
                .color(ink(0x7bc6ff)),
        );
    }

    for action in ShortcutAction::ALL {
        let recording = w.layout.recording_shortcut == Some(action);
        let binding = w.layout.shortcuts.get(action).display();
        let badge_text = if recording {
            "按下按键…".to_owned()
        } else {
            binding.clone()
        };
        shortcut_list = shortcut_list.child(
            row()
                .key(format!("shortcut-{}", action.id()))
                .role("button")
                .label(format!("{}，当前 {binding}", action.label()))
                .help("点击后按下新组合键即可修改，按 Esc 取消")
                .focusable(true)
                .w_full()
                .h(34.)
                .px(10.)
                .gap(8.)
                .items_center()
                .radius(6.)
                .fill(ink(if recording { 0x1a304d } else { 0x16202e }))
                .border(1., ink(if recording { 0x4b93df } else { 0x243041 }))
                .hover(ink(if recording { 0x20395a } else { 0x1d293a }))
                .on_key(Workspace::workbench_key)
                .on_press_tx(move |w: &mut Workspace, tx| w.toggle_record_shortcut(action, tx))
                .child(
                    text(action.label())
                        .font(ui_font(13., 480.))
                        .grow(1.)
                        .min_w(0.)
                        .truncate()
                        .color(ink(TEXT)),
                )
                .child(
                    row()
                        .h(22.)
                        .px(7.)
                        .center()
                        .radius(5.)
                        .fill(ink(if recording { 0x2665a6 } else { 0x253143 }))
                        .child(
                            text(badge_text)
                                .font(ui_font(12., 520.))
                                .color(ink(if recording { 0xffffff } else { 0xd9e4f2 })),
                        ),
                ),
        );
    }

    shortcut_list = shortcut_list.child(
        text(
            "局部快捷键：Esc 关闭搜索或抽屉，抽屉内 Ctrl+F 搜索，终端内 Ctrl+C / Ctrl+V 复制粘贴。",
        )
        .font(ui_font(11.5, 400.))
        .wrap_anywhere()
        .color(ink(MUTED)),
    );

    col()
        .w_full()
        .h_full()
        .min_h(0.)
        .px(16.)
        .pt(16.)
        .pb(16.)
        .gap(14.)
        .on_key(Workspace::workbench_key)
        .child(text("设置").font(ui_font(20., 650.)).color(ink(TEXT)))
        .child(el().h(1.).w_full().fill(ink(LINE)))
        .child(
            col()
                .key("settings-scroll")
                .w_full()
                .grow(1.)
                .min_h(0.)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(
                    col()
                        .w_full()
                        .gap(12.)
                        .child(tray_card)
                        .child(metrics_card)
                        .child(el().h(1.).w_full().fill(ink(LINE)))
                        .child(shortcut_list),
                ),
        )
}
