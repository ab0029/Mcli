use super::{Filter, LINE, MUTED, TEXT, Workspace, button, icons, ink, ui_font};
use crate::clawbot::ConnectionState;
use crate::clawbot::qr::QrMatrix;
use crate::workbench::{ContextMenu, ShortcutAction};
use rune::{
    Color, El, Icon, PopupItem, PopupMenu, ScrollSpec, Shape, canvas, col, el, icon, row, text,
};

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
            w.layout.menu = None;
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
        "在终端上方显示代理快速切换器与整机 CPU、内存与网速监控；关闭可留出更多终端行数并停止后台采样。",
        w.layout.show_metrics_bar,
        Workspace::set_show_metrics_bar,
    );
    let sound_section = sound_settings(w, &toggle_card);
    let proxy_section = proxy_settings(w);
    let clawbot_section = clawbot_settings(w, &toggle_card);

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
                        .child(shortcut_list)
                        .child(el().h(1.).w_full().fill(ink(LINE)))
                        .child(sound_section)
                        .child(el().h(1.).w_full().fill(ink(LINE)))
                        .child(proxy_section)
                        .child(el().h(1.).w_full().fill(ink(LINE)))
                        .child(clawbot_section),
                ),
        )
}

fn sound_settings(
    w: &Workspace,
    toggle_card: &dyn Fn(
        &'static str,
        &'static str,
        &'static str,
        bool,
        fn(&mut Workspace, bool, &mut rune::Tx),
    ) -> El,
) -> El {
    let mut mode_row = row().w_full().gap(5.).items_center();
    for (mode, id, label) in crate::sound::SoundMode::ALL {
        let selected = w.layout.sound.mode == mode;
        mode_row = mode_row.child(
            row()
                .key(format!("sound-mode-{id}"))
                .role("button")
                .label(format!("提示音触发范围：{label}"))
                .focusable(true)
                .h(25.)
                .px(8.)
                .center()
                .radius(6.)
                .fill(ink(if selected { 0x2665a6 } else { 0x202b3a }))
                .hover(ink(if selected { 0x2f74bc } else { 0x2a384b }))
                .on_key(Workspace::workbench_key)
                .on_press_tx(move |w: &mut Workspace, tx| w.set_sound_mode(mode, tx))
                .child(
                    text(label)
                        .font(ui_font(11.5, if selected { 560. } else { 480. }))
                        .color(ink(if selected { 0xffffff } else { TEXT })),
                ),
        );
    }

    let mut preset_row = row().w_full().gap(5.).items_center();
    for (preset, id, label) in crate::sound::SoundPreset::ALL {
        let selected = w.layout.sound.preset == preset;
        preset_row = preset_row.child(
            row()
                .key(format!("sound-preset-{id}"))
                .role("button")
                .label(format!("提示音效：{label}"))
                .focusable(true)
                .h(25.)
                .px(8.)
                .center()
                .radius(6.)
                .fill(ink(if selected { 0x1d3550 } else { 0x202b3a }))
                .border(1., ink(if selected { 0x3b78b8 } else { 0x283649 }))
                .hover(ink(0x273c56))
                .on_key(Workspace::workbench_key)
                .on_press_tx(move |w: &mut Workspace, tx| w.set_sound_preset(preset, tx))
                .child(
                    text(label)
                        .font(ui_font(11.5, if selected { 560. } else { 480. }))
                        .color(ink(if selected { 0xbfe2ff } else { TEXT })),
                ),
        );
    }
    preset_row = preset_row.child(el().grow(1.)).child(
        row()
            .key("sound-preview")
            .role("button")
            .label("试听当前提示音")
            .focusable(true)
            .h(25.)
            .px(9.)
            .center()
            .radius(6.)
            .fill(ink(0x1f2e42))
            .border(1., ink(0x314a69))
            .hover(ink(0x293d57))
            .on_key(Workspace::workbench_key)
            .on_press_tx(|w: &mut Workspace, tx| w.preview_sound(tx))
            .child(text("试听").font(ui_font(11.5, 540.)).color(ink(0x8ec8ff))),
    );

    let card = col()
        .w_full()
        .padding(10.)
        .gap(8.)
        .radius(8.)
        .fill(ink(0x16202e))
        .border(1., ink(0x263244))
        .child(text("触发时机").font(ui_font(12., 540.)).color(ink(TEXT)))
        .child(mode_row)
        .child(
            text("系统提示音效（自动遵循 Windows 静音方案，缺失时安全降级）")
                .font(ui_font(11., 420.))
                .wrap_anywhere()
                .color(ink(MUTED)),
        )
        .child(preset_row);

    let mut section = col()
        .w_full()
        .gap(8.)
        .child(
            col()
                .w_full()
                .gap(3.)
                .child(
                    text("任务完成提示音")
                        .font(ui_font(14., 600.))
                        .color(ink(TEXT)),
                )
                .child(
                    text("Agent 回复完成时播放 Windows 系统提示音（默认仅在窗口失焦或非当前会话时提醒，带 1.5 秒防抖）。")
                        .font(ui_font(11.5, 400.))
                        .wrap_anywhere()
                        .color(ink(MUTED)),
                ),
        )
        .child(card);

    if w.layout.sound.mode != crate::sound::SoundMode::Off {
        section = section.child(toggle_card(
            "setting-sound-waiting",
            "等待用户审批时也播放提示音",
            "Agent 进入「等待用户」确认状态时按相同前后台策略提醒，避免卡在审批界面未察觉。",
            w.layout.sound.on_waiting,
            Workspace::set_sound_on_waiting,
        ));
    }
    section
}

fn proxy_settings(w: &Workspace) -> El {
    let mut section = col().w_full().gap(8.).child(
        col()
            .w_full()
            .gap(3.)
            .child(
                text("代理预设（多环境一键切换）")
                    .font(ui_font(14., 600.))
                    .color(ink(TEXT)),
            )
            .child(
                text("顶部状态栏可一键切换，终端支持 mp <名称|编号|off|sys>，微信支持 /proxy。")
                    .font(ui_font(11.5, 400.))
                    .wrap_anywhere()
                    .color(ink(MUTED)),
            ),
    );

    // Saved profiles list
    let mut list_card = col()
        .w_full()
        .padding(10.)
        .gap(6.)
        .radius(8.)
        .fill(ink(0x16202e))
        .border(1., ink(0x263244));
    if w.layout.proxy.profiles.is_empty() {
        list_card = list_card.child(
            text("暂无代理预设。例如：名称填 p1、地址填 10808（自动补全为 http://127.0.0.1:10808）。")
                .font(ui_font(11.5, 400.))
                .wrap_anywhere()
                .color(ink(MUTED)),
        );
    } else {
        for (idx, p) in w.layout.proxy.profiles.iter().enumerate() {
            let is_default = w
                .layout
                .proxy
                .default_profile
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case(&p.name));
            let name_for_default = p.name.clone();
            let name_for_remove = p.name.clone();
            let safe_url = crate::proxy::sanitize_proxy_summary(&p.url);
            let sub = if p.no_proxy.trim().is_empty() {
                safe_url
            } else {
                format!("{safe_url} · NO_PROXY={}", p.no_proxy.trim())
            };
            list_card = list_card.child(
                row()
                    .key(format!("proxy-profile-{}", p.name))
                    .w_full()
                    .px(8.)
                    .py(6.)
                    .gap(6.)
                    .items_center()
                    .radius(6.)
                    .fill(ink(0x111925))
                    .border(1., ink(if is_default { 0x3778b7 } else { 0x222e40 }))
                    .child(
                        col()
                            .grow(1.)
                            .min_w(0.)
                            .gap(2.)
                            .child(
                                text(format!("[{}] {}", idx + 1, p.name))
                                    .font(ui_font(12.5, 580.))
                                    .truncate()
                                    .color(ink(TEXT)),
                            )
                            .child(
                                text(sub)
                                    .font(ui_font(11., 400.))
                                    .truncate()
                                    .color(ink(MUTED)),
                            ),
                    )
                    .child(
                        row()
                            .key(format!("proxy-default-{}", p.name))
                            .role("button")
                            .label(if is_default {
                                format!("取消默认 {}", p.name)
                            } else {
                                format!("设为默认 {}", p.name)
                            })
                            .focusable(true)
                            .h(22.)
                            .px(7.)
                            .center()
                            .radius(5.)
                            .fill(ink(if is_default { 0x2665a6 } else { 0x202b3a }))
                            .hover(ink(if is_default { 0x2f74bc } else { 0x2a384b }))
                            .on_key(Workspace::workbench_key)
                            .on_press_tx(move |w: &mut Workspace, tx| {
                                let next = if is_default {
                                    None
                                } else {
                                    Some(name_for_default.clone())
                                };
                                w.set_default_proxy_profile(next, tx);
                            })
                            .child(
                                text(if is_default { "默认" } else { "设默认" })
                                    .font(ui_font(11., 520.))
                                    .color(ink(if is_default { 0xffffff } else { TEXT })),
                            ),
                    )
                    .child(
                        row()
                            .key(format!("proxy-delete-{}", p.name))
                            .role("button")
                            .label(format!("删除预设 {}", p.name))
                            .focusable(true)
                            .h(22.)
                            .px(7.)
                            .center()
                            .radius(5.)
                            .fill(ink(0x42252e))
                            .hover(ink(0x562f3b))
                            .on_key(Workspace::workbench_key)
                            .on_press_tx(move |w: &mut Workspace, tx| {
                                w.remove_proxy_profile(name_for_remove.clone(), tx);
                            })
                            .child(text("删除").font(ui_font(11., 520.)).color(ink(0xffb3b5))),
                    ),
            );
        }
    }

    // Add / Update form
    let mut form = col()
        .w_full()
        .gap(6.)
        .child(
            row()
                .w_full()
                .gap(6.)
                .child(
                    row()
                        .w(84.)
                        .h(30.)
                        .px(7.)
                        .items_center()
                        .radius(6.)
                        .fill(ink(0x101722))
                        .border(1., ink(0x2c3a4e))
                        .child(
                            rune::text_input(&w.layout.proxy_name_input, ui_font(12., 400.))
                                .key("proxy-name-input")
                                .label("预设名称")
                                .h(26.)
                                .pt(5.)
                                .min_w(0.)
                                .grow(1.)
                                .single_line()
                                .color(ink(TEXT))
                                .placeholder("名称如 p1", ink(0x7c8a9e))
                                .on_edit(|w: &mut Workspace| &mut w.layout.proxy_name_input),
                        ),
                )
                .child(
                    row()
                        .grow(1.)
                        .min_w(0.)
                        .h(30.)
                        .px(7.)
                        .items_center()
                        .radius(6.)
                        .fill(ink(0x101722))
                        .border(1., ink(0x2c3a4e))
                        .child(
                            rune::text_input(&w.layout.proxy_url_input, ui_font(12., 400.))
                                .key("proxy-url-input")
                                .label("代理地址或端口")
                                .h(26.)
                                .pt(5.)
                                .min_w(0.)
                                .grow(1.)
                                .single_line()
                                .color(ink(TEXT))
                                .placeholder("10808 或 http://127.0.0.1:10808", ink(0x7c8a9e))
                                .on_edit(|w: &mut Workspace| &mut w.layout.proxy_url_input),
                        ),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(6.)
                .items_center()
                .child(
                    row()
                        .grow(1.)
                        .min_w(0.)
                        .h(28.)
                        .px(7.)
                        .items_center()
                        .radius(6.)
                        .fill(ink(0x101722))
                        .border(1., ink(0x2c3a4e))
                        .child(
                            rune::text_input(&w.layout.proxy_no_proxy_input, ui_font(11.5, 400.))
                                .key("proxy-noproxy-input")
                                .label("可选 NO_PROXY")
                                .h(24.)
                                .pt(4.)
                                .min_w(0.)
                                .grow(1.)
                                .single_line()
                                .color(ink(TEXT))
                                .placeholder(
                                    "NO_PROXY（可选，如 localhost,127.0.0.1）",
                                    ink(0x7c8a9e),
                                )
                                .on_edit(|w: &mut Workspace| &mut w.layout.proxy_no_proxy_input),
                        ),
                )
                .child(
                    row()
                        .key("proxy-add-btn")
                        .role("button")
                        .label("保存代理预设")
                        .focusable(true)
                        .h(28.)
                        .px(10.)
                        .center()
                        .radius(6.)
                        .fill(ink(0x2665a6))
                        .hover(ink(0x2f74bc))
                        .on_key(Workspace::workbench_key)
                        .on_press_tx(|w: &mut Workspace, tx| w.add_proxy_profile(tx))
                        .child(
                            text("保存预设")
                                .font(ui_font(12., 560.))
                                .color(ink(0xffffff)),
                        ),
                ),
        );
    if let Some(notice) = &w.layout.proxy_notice {
        form = form.child(
            text(notice)
                .font(ui_font(11.5, 430.))
                .wrap_anywhere()
                .color(ink(0xffce77)),
        );
    }
    list_card = list_card.child(form);
    section = section.child(list_card);

    // Global default & per-Agent auto defaults
    let current_default = w.layout.proxy.default_profile.as_deref();
    let default_label = match current_default {
        None => "系统默认".to_owned(),
        Some(v) if v == crate::proxy::DIRECT_PROFILE => "直连（清除代理）".to_owned(),
        Some(name) => name.to_owned(),
    };
    let mut rules_card = col()
        .w_full()
        .padding(10.)
        .gap(6.)
        .radius(8.)
        .fill(ink(0x16202e))
        .border(1., ink(0x263244))
        .child(
            row()
                .w_full()
                .items_center()
                .gap(8.)
                .child(
                    text("新终端默认代理")
                        .font(ui_font(12.5, 560.))
                        .grow(1.)
                        .min_w(0.)
                        .truncate()
                        .color(ink(TEXT)),
                )
                .child(
                    row()
                        .key("proxy-global-default")
                        .role("button")
                        .label(format!("新终端默认代理：{default_label}"))
                        .focusable(true)
                        .h(24.)
                        .px(8.)
                        .gap(5.)
                        .items_center()
                        .radius(5.)
                        .fill(ink(0x202b3a))
                        .border(1., ink(0x2f4058))
                        .hover(ink(0x2a384b))
                        .on_key(Workspace::workbench_key)
                        .on_press_tx(|w: &mut Workspace, tx| {
                            tx.instant();
                            if matches!(w.layout.menu, Some(ContextMenu::ProxyGlobalDefault)) {
                                w.layout.menu = None;
                            } else {
                                w.layout.menu = Some(ContextMenu::ProxyGlobalDefault);
                            }
                        })
                        .child(
                            text(default_label)
                                .font(ui_font(11.5, 520.))
                                .color(ink(0xd9e4f2)),
                        )
                        .child(icon(&icons::DOWN, 10.).color(ink(0x8da3bd))),
                ),
        )
        .child(
            text("按 Agent 自动默认代理（在未手动 mp / 修改环境变量时，启动对应 CLI 前自动生效）")
                .font(ui_font(11., 400.))
                .wrap_anywhere()
                .color(ink(MUTED)),
        );

    for (agent, display) in [
        ("codex", "Codex"),
        ("agy", "agy"),
        ("kimi", "Kimi"),
        ("grok", "Grok"),
    ] {
        let cur_val = w.layout.proxy.agent_default(agent);
        let cur_label = match cur_val {
            None => "跟随终端".to_owned(),
            Some(v) if v == crate::proxy::DIRECT_PROFILE => "直连".to_owned(),
            Some(name) => name.to_owned(),
        };
        let btn_key = format!("proxy-agent-{agent}");
        let btn_key_cb = btn_key.clone();
        rules_card = rules_card.child(
            row()
                .w_full()
                .h(26.)
                .items_center()
                .gap(8.)
                .child(
                    text(format!("启动 {display} 时"))
                        .font(ui_font(12., 480.))
                        .grow(1.)
                        .min_w(0.)
                        .truncate()
                        .color(ink(TEXT)),
                )
                .child(
                    row()
                        .key(btn_key)
                        .role("button")
                        .label(format!("{display} 自动代理：{cur_label}"))
                        .focusable(true)
                        .h(22.)
                        .px(8.)
                        .gap(5.)
                        .items_center()
                        .radius(5.)
                        .fill(ink(if cur_val.is_some() {
                            0x1d3550
                        } else {
                            0x202b3a
                        }))
                        .border(
                            1.,
                            ink(if cur_val.is_some() {
                                0x356496
                            } else {
                                0x2c3c52
                            }),
                        )
                        .hover(ink(0x2a3d56))
                        .on_key(Workspace::workbench_key)
                        .on_press_tx(move |w: &mut Workspace, tx| {
                            tx.instant();
                            let next = ContextMenu::ProxyAgentDefault {
                                agent,
                                anchor: btn_key_cb.clone(),
                            };
                            if w.layout.menu.as_ref() == Some(&next) {
                                w.layout.menu = None;
                            } else {
                                w.layout.menu = Some(next);
                            }
                        })
                        .child(
                            text(cur_label)
                                .font(ui_font(11.5, 500.))
                                .color(ink(if cur_val.is_some() { 0x9cd2ff } else { MUTED })),
                        )
                        .child(icon(&icons::DOWN, 10.).color(ink(0x8da3bd))),
                ),
        );
    }

    section.child(rules_card)
}

fn qr_view(matrix: &QrMatrix) -> El {
    const DISPLAY_PX: f32 = 168.;
    const QUIET: usize = 3;
    let total_modules = (matrix.size + QUIET * 2) as f32;
    let cell = DISPLAY_PX / total_modules;
    let mut shape = Shape::new();
    for y in 0..matrix.size {
        let mut x = 0;
        while x < matrix.size {
            if !matrix.get(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < matrix.size && matrix.get(x, y) {
                x += 1;
            }
            let x0 = (start + QUIET) as f32 * cell;
            let x1 = (x + QUIET) as f32 * cell;
            let y0 = (y + QUIET) as f32 * cell;
            let y1 = (y + QUIET + 1) as f32 * cell;
            shape = shape.fill([[x0, y0], [x1, y0], [x1, y1], [x0, y1]], Color::BLACK);
        }
    }
    row()
        .key("clawbot-qr-code")
        .size(DISPLAY_PX, DISPLAY_PX)
        .radius(8.)
        .fill(Color::WHITE)
        .center()
        .child(canvas(shape).size(DISPLAY_PX, DISPLAY_PX))
}

fn action_btn(
    key: &'static str,
    label: &'static str,
    bg: u32,
    fg: u32,
    act: fn(&mut Workspace, &mut rune::Tx),
) -> El {
    row()
        .key(key)
        .role("button")
        .label(label)
        .focusable(true)
        .h(26.)
        .px(10.)
        .center()
        .radius(6.)
        .fill(ink(bg))
        .hover(ink(bg.saturating_add(0x0d1218)))
        .on_key(Workspace::workbench_key)
        .on_press_tx(act)
        .child(text(label).font(ui_font(12., 550.)).color(ink(fg)))
}

fn clawbot_settings(
    w: &Workspace,
    toggle_card: &impl Fn(
        &'static str,
        &'static str,
        &'static str,
        bool,
        fn(&mut Workspace, bool, &mut rune::Tx),
    ) -> El,
) -> El {
    let enabled = w.layout.clawbot.enabled;
    let state = &w.clawbot.state;
    let (badge_bg, badge_fg) = match state {
        ConnectionState::Connected { paused: false, .. } => (0x193d2b, 0x9ae89f),
        ConnectionState::Connected { paused: true, .. } => (0x352d49, 0xceb5ff),
        ConnectionState::WaitingQr { .. } | ConnectionState::Reconnecting { .. } => {
            (0x103557, 0x7bc6ff)
        }
        ConnectionState::TakenByOtherWindow => (0x493620, 0xffce77),
        ConnectionState::SessionExpired { .. } | ConnectionState::Error(_) => (0x472831, 0xff9b9e),
        ConnectionState::Disabled | ConnectionState::Unbound => (0x27313e, 0xb6c1cf),
    };

    let header = row()
        .w_full()
        .items_center()
        .justify_between()
        .child(
            text("微信 ClawBot 远程控制")
                .font(ui_font(14., 600.))
                .color(ink(TEXT)),
        )
        .child(
            row()
                .key("clawbot-status-badge")
                .h(20.)
                .px(8.)
                .center()
                .radius(9.)
                .fill(ink(badge_bg))
                .child(
                    text(state.badge_text())
                        .font(ui_font(11.5, 560.))
                        .color(ink(badge_fg)),
                ),
        );

    let enable_card = toggle_card(
        "setting-clawbot-enabled",
        "启用微信 ClawBot",
        "开启后由本机后台长轮询连接微信 iLink，凭据使用 Windows DPAPI 加密保存，仅响应绑定账号。",
        enabled,
        Workspace::set_clawbot_enabled,
    );

    let mut section = col().w_full().gap(8.).child(header).child(enable_card);

    if !enabled {
        return section;
    }

    // Status details & primary action buttons
    let mut panel = col()
        .w_full()
        .padding(11.)
        .gap(8.)
        .radius(8.)
        .fill(ink(0x16202e))
        .border(1., ink(0x263244));

    match state {
        ConnectionState::Unbound => {
            panel = panel
                .child(
                    text("请先在手机微信「我 -> 设置 -> 插件」开启微信ClawBot，然后点击下方按钮扫码绑定。")
                        .font(ui_font(12., 400.))
                        .wrap_anywhere()
                        .color(ink(MUTED)),
                )
                .child(row().gap(8.).child(action_btn(
                    "clawbot-qr-start",
                    "生成扫码二维码",
                    0x2665a6,
                    0xffffff,
                    Workspace::clawbot_start_qr,
                )));
        }
        ConnectionState::WaitingQr { scanned, .. } => {
            let hint = if *scanned {
                "手机已扫码，请在微信上点击「确认授权」…"
            } else {
                "请使用手机微信扫描下方二维码完成绑定："
            };
            panel = panel.child(
                text(hint)
                    .font(ui_font(12., 500.))
                    .wrap_anywhere()
                    .color(ink(0x7bc6ff)),
            );
            if let Some(matrix) = &w.clawbot.qr_matrix {
                panel = panel.child(row().w_full().center().child(qr_view(matrix)));
            }
            panel = panel.child(
                row()
                    .gap(8.)
                    .child(action_btn(
                        "clawbot-qr-start",
                        "刷新二维码",
                        0x223349,
                        TEXT,
                        Workspace::clawbot_start_qr,
                    ))
                    .child(action_btn(
                        "clawbot-qr-cancel",
                        "取消",
                        0x2a3444,
                        MUTED,
                        Workspace::clawbot_cancel_qr,
                    )),
            );
        }
        ConnectionState::Connected { user_hint, paused } => {
            panel = panel
                .child(
                    text(format!("已绑定微信：{user_hint}"))
                        .font(ui_font(12.5, 520.))
                        .color(ink(0x9ae89f)),
                )
                .child(
                    row()
                        .gap(8.)
                        .child(action_btn(
                            "clawbot-pause",
                            if *paused {
                                "恢复接收"
                            } else {
                                "暂停接收"
                            },
                            0x223349,
                            TEXT,
                            Workspace::clawbot_toggle_pause,
                        ))
                        .child(action_btn(
                            "clawbot-unbind",
                            "解除绑定",
                            0x472831,
                            0xffb3b5,
                            Workspace::clawbot_unbind,
                        )),
                );
        }
        ConnectionState::TakenByOtherWindow => {
            panel = panel
                .child(
                    text("当前已有另一个 Mcli 窗口持有微信长轮询连接。为避免微信单连接互踢，本窗口处于只读待命状态。")
                        .font(ui_font(12., 400.))
                        .wrap_anywhere()
                        .color(ink(0xffce77)),
                )
                .child(row().gap(8.).child(action_btn(
                    "clawbot-claim",
                    "尝试接管连接",
                    0x223349,
                    TEXT,
                    Workspace::clawbot_claim_ownership,
                )));
        }
        ConnectionState::Reconnecting {
            user_hint,
            attempt,
            reason,
        } => {
            panel = panel
                .child(
                    text(format!(
                        "账号 {user_hint} 正在第 {attempt} 次重连：{reason}"
                    ))
                    .font(ui_font(12., 420.))
                    .wrap_anywhere()
                    .color(ink(0x7bc6ff)),
                )
                .child(row().gap(8.).child(action_btn(
                    "clawbot-unbind",
                    "解除绑定",
                    0x472831,
                    0xffb3b5,
                    Workspace::clawbot_unbind,
                )));
        }
        ConnectionState::SessionExpired { reason } => {
            panel = panel
                .child(
                    text(format!("会话已失效：{reason}"))
                        .font(ui_font(12., 450.))
                        .wrap_anywhere()
                        .color(ink(0xff9b9e)),
                )
                .child(
                    row()
                        .gap(8.)
                        .child(action_btn(
                            "clawbot-qr-start",
                            "重新扫码",
                            0x2665a6,
                            0xffffff,
                            Workspace::clawbot_start_qr,
                        ))
                        .child(action_btn(
                            "clawbot-unbind",
                            "清除凭据",
                            0x472831,
                            0xffb3b5,
                            Workspace::clawbot_unbind,
                        )),
                );
        }
        ConnectionState::Error(err) => {
            panel = panel
                .child(
                    text(err)
                        .font(ui_font(12., 420.))
                        .wrap_anywhere()
                        .color(ink(0xff9b9e)),
                )
                .child(row().gap(8.).child(action_btn(
                    "clawbot-qr-start",
                    "重新生成二维码",
                    0x2665a6,
                    0xffffff,
                    Workspace::clawbot_start_qr,
                )));
        }
        ConnectionState::Disabled => {}
    }

    if let Some(notice) = &w.clawbot.notice {
        panel = panel.child(
            text(notice)
                .font(ui_font(11.5, 420.))
                .wrap_anywhere()
                .color(ink(0xffce77)),
        );
    }
    section = section.child(panel);

    // Default routing config (default agent + default directory)
    let mut agent_chips = row().w_full().gap(5.).items_center();
    let current_agent = w.layout.clawbot.normalized_agent();
    for (opt, key, label) in [
        (None, "clawbot-agent-none", "无"),
        (Some("codex"), "clawbot-agent-codex", "Codex"),
        (Some("agy"), "clawbot-agent-agy", "agy"),
        (Some("kimi"), "clawbot-agent-kimi", "Kimi"),
        (Some("grok"), "clawbot-agent-grok", "Grok"),
    ] {
        let selected = current_agent == opt;
        agent_chips = agent_chips.child(
            row()
                .key(key)
                .role("button")
                .label(format!("默认 Agent：{label}"))
                .focusable(true)
                .h(24.)
                .px(7.)
                .center()
                .radius(6.)
                .fill(ink(if selected { 0x2665a6 } else { 0x202b3a }))
                .hover(ink(if selected { 0x2f74bc } else { 0x2a384b }))
                .on_key(Workspace::workbench_key)
                .on_press_tx(move |w: &mut Workspace, tx| {
                    w.set_clawbot_default_agent(opt.map(str::to_owned), tx);
                })
                .child(
                    text(label)
                        .font(ui_font(11.5, 540.))
                        .color(ink(if selected { 0xffffff } else { TEXT })),
                ),
        );
    }

    let routing_card = col()
        .w_full()
        .padding(11.)
        .gap(7.)
        .radius(8.)
        .fill(ink(0x16202e))
        .border(1., ink(0x263244))
        .child(
            text("默认路由（未 /use 绑定时）")
                .font(ui_font(12.5, 580.))
                .color(ink(TEXT)),
        )
        .child(agent_chips)
        .child(
            row()
                .w_full()
                .h(30.)
                .px(8.)
                .items_center()
                .gap(6.)
                .radius(6.)
                .fill(ink(0x101722))
                .border(1., ink(0x2c3a4e))
                .child(
                    rune::text_input(&w.layout.clawbot_default_dir_input, ui_font(12., 400.))
                        .key("clawbot-default-dir")
                        .label("默认项目目录")
                        .h(26.)
                        .pt(5.)
                        .min_w(0.)
                        .grow(1.)
                        .single_line()
                        .color(ink(TEXT))
                        .placeholder("默认项目绝对路径（可选）", ink(0x7c8a9e))
                        .on_edit(|w: &mut Workspace| &mut w.layout.clawbot_default_dir_input)
                        .on_input_change(|w: &mut Workspace, tx| w.commit_clawbot_default_dir(tx)),
                ),
        )
        .child(
            text("接口 Base URL（默认官方，可改填本地 ilink-hub）")
                .font(ui_font(11.5, 450.))
                .color(ink(MUTED)),
        )
        .child(
            row()
                .w_full()
                .h(30.)
                .px(8.)
                .items_center()
                .gap(6.)
                .radius(6.)
                .fill(ink(0x101722))
                .border(1., ink(0x2c3a4e))
                .child(
                    rune::text_input(&w.layout.clawbot_base_url_input, ui_font(12., 400.))
                        .key("clawbot-base-url")
                        .label("微信 iLink Base URL")
                        .h(26.)
                        .pt(5.)
                        .min_w(0.)
                        .grow(1.)
                        .single_line()
                        .color(ink(TEXT))
                        .placeholder("https://ilinkai.weixin.qq.com", ink(0x7c8a9e))
                        .on_edit(|w: &mut Workspace| &mut w.layout.clawbot_base_url_input)
                        .on_input_change(|w: &mut Workspace, tx| w.commit_clawbot_base_url(tx)),
                ),
        );
    section = section.child(routing_card);

    section = section.child(toggle_card(
        "setting-clawbot-keepalive",
        "会话令牌到期前保活提醒",
        "微信会话令牌自你上次发言起有效约 24 小时；开启后在到期前 3 小时、2 小时、1 小时主动发微信提醒，回复 /ping 即可续期 24 小时。",
        w.layout.clawbot.keepalive_remind,
        Workspace::set_clawbot_keepalive_remind,
    ));

    section = section.child(toggle_card(
        "setting-clawbot-notify-desktop",
        "桌面端任务完成也推送微信",
        "默认仅回传手机微信发起的任务；开启后，桌面手动触发的 Agent 任务完成或等待确认也会推送。",
        w.layout.clawbot.notify_desktop_tasks,
        Workspace::set_clawbot_notify_desktop_tasks,
    ));

    section = section.child(toggle_card(
        "setting-clawbot-raw-shell",
        "允许 /sh 执行 PowerShell 命令",
        "高权限开关（默认关闭）。开启后允许在普通 PowerShell 终端通过 /sh <命令> 执行单条命令。",
        w.layout.clawbot.allow_raw_shell,
        Workspace::set_clawbot_allow_raw_shell,
    ));

    if !w.clawbot.audit.is_empty() {
        let mut log_col = col()
            .w_full()
            .padding(10.)
            .gap(4.)
            .radius(8.)
            .fill(ink(0x121925))
            .border(1., ink(0x222e3f))
            .child(
                text(format!("最近远程操作审计（{}）", w.clawbot.audit.len()))
                    .font(ui_font(12., 580.))
                    .color(ink(TEXT)),
            );
        for entry in w.clawbot.audit.iter().take(8) {
            log_col = log_col.child(
                text(format!(
                    "{} [{}] {} · {}",
                    entry.time, entry.kind, entry.target, entry.outcome
                ))
                .font(ui_font(11., 400.))
                .wrap_anywhere()
                .color(ink(MUTED)),
            );
        }
        section = section.child(log_col);
    }

    section
}
