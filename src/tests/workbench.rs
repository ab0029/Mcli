use super::conversation_index::{fixture as index_fixture, key, prompt, put, source};
use super::*;
use crate::conversation_store::Store;
use crate::discovery::Catalog;

#[test]
fn agy_activity_hint_must_belong_to_the_cursor_composer() {
    let mut term = Term::new(Config::default(), &Size::default(), Replies::default());
    let mut parser = Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();
    let divider = "─".repeat(80);
    let view =
        format!("\x1b[3;1H{divider}\x1b[4;1H> \x1b[5;1H{divider}\x1b[6;1Hesc to cancel\x1b[4;3H");
    parser.advance(&mut term, view.as_bytes());
    assert_eq!(crate::terminal::agy_composer_busy(&term), Some(true));
    parser.advance(&mut term, b"\x1b[6;1H\x1b[2K? for shortcuts\x1b[4;3H");
    assert_eq!(crate::terminal::agy_composer_busy(&term), Some(false));
    parser.advance(&mut term, b"\x1b[1;1H");
    assert_eq!(crate::terminal::agy_composer_busy(&term), None);
    parser.advance(&mut term, b"\x1b[3;1H\x1b[2K\x1b[4;3H");
    assert_eq!(crate::terminal::agy_composer_busy(&term), None);
}

#[test]
fn native_prompts_and_provider_errors_need_a_current_structured_screen() {
    let state = |agent, screen: &str| {
        let mut term = Term::new(
            Config::default(),
            &Size {
                cols: 110,
                lines: 32,
                ..Size::default()
            },
            Replies::default(),
        );
        let mut parser = Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();
        parser.advance(&mut term, screen.replace('\n', "\r\n").as_bytes());
        crate::terminal::screen_state(&term, agent)
    };
    let agy = shell::Foreground::Agy;
    let codex = shell::Foreground::Codex;
    let agy_gate = "Requesting permission for:\nRun this command?\n> 1. Yes, run command\n  4. No, cancel\n  ↑/↓ Navigate · tab Amend";
    let codex_gate = "Would you like to run the following command?\n› 1. Yes, proceed (y)\n  3. No, and tell Codex what to do differently (esc)\nPress enter to confirm or esc to cancel";
    assert_eq!(state(agy, agy_gate), Some("等待用户"));
    assert_eq!(state(codex, codex_gate), Some("等待用户"));
    assert_eq!(state(agy, "The output says: No, cancel\nNavigate"), None);
    assert_eq!(
        state(agy, "You declined the tool call.\n> Auto mode:"),
        None
    );
    let failure = "⚠ FAILED_PRECONDITION (code 400): Service unavailable.\nError ID: e1df5456-6d73-4e0b-8e01-f8a56d07e222-1\n> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)";
    assert_eq!(state(agy, failure), Some("出错"));
    assert_eq!(state(codex, failure), None);
    assert_eq!(
        state(agy, &format!("{failure}\n> second user submission")),
        None
    );
    let id = key("agy", 1);
    for (pending, state, want) in [
        (true, "working", "waiting"),
        (false, "tool_use", "working"),
        (false, "idle", "ready"),
        (false, "error", "error"),
        (false, "initializing", "online"),
    ] {
        let item=crate::hook_wire::Notice::from_hook("agy","StatusLine", SessionId::new().marker(),"0123456789abcdef0123456789abcdef".into(),
            &serde_json::json!({"conversation_id":id.id,"agent_state":state,"tool_confirmation_pending":pending})).unwrap();
        assert_eq!(item.detail, want);
        assert_eq!(item.id, id.id);
    }
}

fn ctrl(key: Key, shift: bool) -> KeyEvent {
    KeyEvent {
        key,
        mods: Mods {
            cmd: true,
            ctrl: true,
            shift,
            ..Default::default()
        },
    }
}

#[test]
fn brand_stays_in_the_window_frame_and_leaves_navigation_and_caption_controls_free() {
    for width in [410., 760., 1699.] {
        let mut rt = app().headless_runtime((width, 680.), rune::Env::new(true, 1.));
        rt.update(|w, _| w.first_open = false);
        rt.frame(0.1);
        assert_eq!(
            rt.state.caption_brand.get().unwrap().wait().unwrap(),
            [288, 96]
        );
        rt.frame(0.2);
        let [x, y, w, h] = rt.presented("caption-brand").unwrap().0;
        let [_, top, _, height] = rt.presented("workspace-titlebar").unwrap().0;
        let [_, body_y, _, _] = rt.presented("workbench").unwrap().0;
        assert_eq!((w, h), (96., 32.));
        assert!(y >= top && y + h <= body_y);
        assert!((body_y - top - height).abs() <= 1.);
        let projects = rt.presented("nav-projects").unwrap().0;
        assert!((projects[1] - body_y - 16.).abs() <= 1.);
        if cfg!(windows) {
            let controls = rt.presented("rune-caption-minimize").unwrap().0;
            assert!(x + w < controls[0]);
            let description = rt.presented("caption-description");
            if width > 410. {
                let [left, _, width, _] = description.unwrap().0;
                assert!(left + width <= controls[0]);
            } else {
                assert!(description.is_none());
            }
        }
        let elements = rt.elements();
        let item = |key| {
            elements
                .iter()
                .find(|e| e.key.as_deref() == Some(key))
                .unwrap()
        };
        assert!(!item("caption-brand").focusable);
        assert!(item("nav-projects").focusable && item("nav-settings").focusable);
        for at in [x + 14., width * 0.5] {
            let cy = y + h / 2.;
            rt.pointer_down(at, cy);
            assert_eq!(rt.take_titlebar_action(), Some(rune::TitlebarAction::Drag));
            rt.pointer_up(at, cy);
            rt.pointer_down(at, cy);
            assert_eq!(
                rt.take_titlebar_action(),
                Some(rune::TitlebarAction::DoubleClick)
            );
            rt.pointer_up(at, cy);
        }
        press(&mut rt, "nav-settings", 0.25);
        assert!(rt.state.layout.settings);
        assert_eq!(rt.presented("caption-brand").unwrap().0, [x, y, w, h]);
        press(&mut rt, "nav-projects", 0.3);
        assert!(!rt.state.layout.settings);
    }
}

#[test]
fn navigation_search_and_filters_keep_the_terminal_and_background_reply_independent() {
    let (dir, _, _) = index_fixture("sidebar-navigation");
    let mut rt = app().headless_runtime((1120., 720.), rune::Env::new(true, 1.));
    rt.update(|w, _| w.first_open = false);
    let shell = super::multiple::launch_profile_shell(&mut rt, &dir);
    let ready = SessionId::new();
    let running = SessionId::new();
    rt.update(|w, _| {
        for (id, name, phase) in [(ready, "review", "就绪"), (running, "build", "运行中")] {
            let mut tab =
                crate::workspace::Tab::pending(id, dir.join(name), std::sync::mpsc::channel().1);
            tab.opening = None;
            tab.foreground = Some(crate::shell::Foreground::Codex);
            tab.phase = phase.into();
            tab.unread = id == ready;
            tab.prompt_count = 1;
            tab.preview = if id == ready {
                "核对账单"
            } else {
                "等待编译"
            }
            .into();
            w.tabs.push(tab);
        }
    });
    rt.frame(0.2);
    let ready_row = format!("session-{}", ready.0);
    let running_row = format!("session-{}", running.0);
    press(&mut rt, "nav-settings", 0.25);
    assert!(
        rt.state.layout.settings,
        "设置按钮的实际位置 {:?}",
        rt.presented("nav-settings")
    );
    assert!(
        rt.elements()
            .iter()
            .all(|e| e.key.as_deref() != Some("sidebar-search"))
    );
    assert_eq!(rt.state.active, Some(shell));
    rt.key(ctrl(Key::Command('k'), true));
    rt.frame(0.2);
    assert!(!rt.state.layout.settings);
    rt.ime(Ime::Commit("账单".into()));
    rt.frame(0.2);
    assert_eq!(rt.state.layout.search.text, "账单");
    assert!(rt.presented(&ready_row).is_some() && rt.presented(&running_row).is_none());
    assert!(!frame(rt.state.session().unwrap()).contains("账单"));
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Default::default(),
    });
    rt.frame(0.2);
    assert!(rt.state.layout.search.text.is_empty());
    assert!(rt.presented(&running_row).is_some());
    press(&mut rt, "filter-running", 0.2);
    assert!(rt.presented(&running_row).is_some() && rt.presented(&ready_row).is_none());
    press(&mut rt, "filter-finished", 0.2);
    assert!(rt.presented(&running_row).is_none() && rt.presented(&ready_row).is_some());
    assert!(rt.presented(&format!("session-{}", shell.0)).is_none());
    press(&mut rt, "filter-unread", 0.2);
    press(&mut rt, &ready_row, 0.2);
    assert_eq!(rt.state.active, Some(ready));
    assert!(rt.presented(&ready_row).is_none());
    assert!(!rt.state.tabs.iter().find(|t| t.id == ready).unwrap().unread);
}

#[test]
fn terminal_arrows_edit_the_draft_without_drawing_a_focus_ring() {
    let (dir, _, _) = index_fixture("terminal-arrow-focus");
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    rt.update(|w, _| w.first_open = false);
    let clock = Instant::now();
    super::multiple::launch_profile_shell(&mut rt, &dir);

    let has_outline = |rt: &rune::Runtime<Workspace>, key: &str| {
        let bounds = rt.presented(key).unwrap().0;
        rt.instances.iter().any(|draw| {
            draw.fill == [0.; 4]
                && draw.border[3] > 0.
                && draw
                    .rect
                    .iter()
                    .zip(bounds)
                    .all(|(a, b)| (a - b).abs() <= 4.)
        })
    };
    rt.ime(Ime::Commit("Mcli-AD".into()));
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).contains("Mcli-AD")
    });
    for (key, insert, expected) in [(Key::Left, "B", "Mcli-ABD"), (Key::Right, "E", "Mcli-ABDE")] {
        rt.key(KeyEvent {
            key,
            mods: Default::default(),
        });
        rt.frame(0.016);
        assert!(!has_outline(&rt, "terminal"), "方向键触发了终端整圈描边");
        rt.ime(Ime::Commit(insert.into()));
        wait_for_workspace(&mut rt, clock, |w| {
            frame(w.session().unwrap()).contains(expected)
        });
    }
    rt.update(|_, tx| tx.focus("footprint-toggle", true));
    rt.frame(0.016);
    assert!(
        has_outline(&rt, "footprint-toggle"),
        "侧栏按钮应显示键盘焦点"
    );
}

#[test]
fn shortcuts_keep_the_drawer_input_and_next_terminal_independent() {
    let (dir, _, _) = index_fixture("workbench-keys");
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    rt.update(|w, _| w.first_open = false);
    let clock = Instant::now();
    let a = super::multiple::launch_profile_shell(&mut rt, &dir);
    wait_for_workspace(&mut rt, clock, |w| w.focused);
    rt.update(|w, _| {
        let t = &mut w.tabs[0];
        t.bound = Some(key("codex", 1));
        t.prompt_count = 1;
    });
    let h = ctrl(Key::Command('h'), true);
    rt.key(h.clone());
    rt.repeat_key(h.clone());
    rt.frame(clock.elapsed().as_secs_f64());
    assert!(rt.presented("prompt-search").is_some());
    assert!(
        rt.elements()
            .iter()
            .any(|e| e.focused && e.key.as_deref() == Some("prompt-search")),
        "抽屉打开后焦点 {:?}",
        rt.elements()
            .iter()
            .filter(|e| e.focused)
            .map(|e| &e.key)
            .collect::<Vec<_>>()
    );
    rt.ime(Ime::Commit("只在抽屉".into()));
    rt.frame(clock.elapsed().as_secs_f64());
    assert!(!frame(rt.state.session().unwrap()).contains("只在抽屉"));
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Default::default(),
    });
    rt.frame(clock.elapsed().as_secs_f64());
    assert!(
        rt.presented("footprints").is_none(),
        "Esc 后焦点 {:?}",
        rt.elements()
            .iter()
            .filter(|e| e.focused)
            .map(|e| &e.key)
            .collect::<Vec<_>>()
    );
    rt.ime(Ime::Commit("终端草稿".into()));
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).contains("终端草稿")
    });
    let new = ctrl(Key::Command('t'), true);
    rt.key(new.clone());
    rt.repeat_key(new);
    rt.frame(clock.elapsed().as_secs_f64());
    wait_for_workspace(&mut rt, clock, |w| {
        w.tabs.len() == 2 && w.active != Some(a) && w.focused
    });
    rt.key(ctrl(Key::PageUp, false));
    rt.frame(clock.elapsed().as_secs_f64());
    assert_eq!(rt.state.active, Some(a));
    assert!(frame(rt.state.session().unwrap()).contains("终端草稿"));
    rt.key(ctrl(Key::Command('l'), true));
    rt.frame(clock.elapsed().as_secs_f64());
    assert!(
        rt.elements()
            .iter()
            .any(|e| e.key.as_deref() == Some("history-toggle")
                && e.label.as_deref() == Some("收起历史"))
    );
    rt.key(ctrl(Key::Command('l'), true));
    rt.frame(clock.elapsed().as_secs_f64());
    wait_for_workspace(&mut rt, clock, |w| w.focused);
}

#[test]
fn splitter_updates_real_columns_and_saves_only_an_intentional_width() {
    let (dir, _, _) = index_fixture("workbench-width");
    let prefs = dir.join("preferences.json");
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    rt.update(|w, _| {
        w.first_open = false;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(&prefs));
    });
    let clock = Instant::now();
    super::multiple::launch_profile_shell(&mut rt, &dir);
    wait_for_workspace(&mut rt, clock, |w| w.focused);
    let original = rt.state.size.cols;
    let [x, y, width, height] = rt.presented("sidebar-divider").unwrap().0;
    let (x, y) = (x + width * 0.5, y + height * 0.5);
    rt.pointer_down(x, y);
    rt.pointer_move(x + 72., y);
    rt.frame(clock.elapsed().as_secs_f64());
    assert!(
        rt.state.layout.width(rt.state.cell) > 248.,
        "拖动区域 {x},{y}，宽度 {}，网格 {}，原始 {original}",
        rt.state.layout.width(rt.state.cell),
        rt.state.size.cols
    );
    wait_for_workspace(&mut rt, clock, |w| w.size.cols < original);
    assert!(!prefs.exists());
    rt.pointer_up(x + 72., y);
    rt.frame(clock.elapsed().as_secs_f64());
    let saved = || rune::prefs::Prefs::at(&prefs).get::<f32>("sidebar_width");
    wait_for_workspace(&mut rt, clock, |_| saved() == Some(320.));
    assert_eq!(
        rt.state.session().unwrap().term.lock().unwrap().columns(),
        rt.state.size.cols as usize
    );
    rt.update(|_, tx| tx.focus("sidebar-divider", true));
    rt.frame(clock.elapsed().as_secs_f64());
    rt.key(KeyEvent {
        key: Key::Left,
        mods: Default::default(),
    });
    rt.frame(clock.elapsed().as_secs_f64());
    wait_for_workspace(&mut rt, clock, |_| saved() == Some(312.));
    let restored = crate::workbench::Layout::new(rune::prefs::Prefs::at(&prefs));
    assert_eq!(restored.width(9.), 312.);
    rt.frame(0.5);
    let [left, top, width, height] = rt.presented("sidebar-divider").unwrap().0;
    let (cx, cy) = (left + width * 0.5, top + height * 0.5);
    for dt in [0.01, 0.08] {
        rt.frame(dt);
        rt.pointer_down(cx, cy);
        rt.pointer_up(cx, cy);
    }
    rt.frame(0.02);
    wait_for_workspace(&mut rt, clock, |_| saved().is_none());
    assert_eq!(rt.state.layout.width(rt.state.cell), 248.);
    rt.set_viewport((1672., 916.), 1.);
    rt.frame(0.2);
    assert!(
        rt.state.layout.width(rt.state.cell) > 370.,
        "窗口 {:?}，项目区 {}，分隔线 {:?}",
        rt.state.layout.viewport.get(),
        rt.state.layout.width(rt.state.cell),
        rt.presented("sidebar-divider")
    );
}

#[test]
fn discovers_unregistered_records_in_descendants_and_preserves_restore_directories() {
    let (dir, roots, db) = index_fixture("workbench-discovery");
    let project = dir.join("含 空格的项目");
    let sub = project.join("child");
    let other = dir.join("another").join("含 空格的项目");
    let mut original = None;
    for n in 0..106 {
        let k = key("codex", n);
        let file = source(&roots, &k);
        let cwd = if n == 0 { &sub } else { &other };
        put(
            &file,
            serde_json::json!({"type":"session_meta","payload":{"id":k.id,"cwd":cwd}}),
        );
        put(
            &file,
            prompt(&k, 1, "official question", Some("2026-11-22T08:00:00Z")),
        );
        if n == 0 {
            original = Some(k);
        }
    }
    let ag = key("agy", 1);
    let uri = serde_json::Value::String(format!(
        "file:///{}",
        sub.to_string_lossy().replace('\\', "/").replace(' ', "%20")
    ));
    put(
        &source(&roots, &ag),
        prompt(&ag, 1, "agy question", Some("2026-11-22T09:00:00Z")),
    );
    put(
        &dir.join("history.jsonl"),
        serde_json::json!({"conversationId":ag.id,"workspace":uri}),
    );
    let mut store = Store::open(&db, roots.clone()).unwrap();
    let mut catalog = Catalog::default();
    catalog.request(project.clone(), &roots);
    let deadline = Instant::now() + Duration::from_secs(12);
    while catalog.scanning() {
        catalog.poll();
        while let Some(s) = catalog.pending.pop_front() {
            while store.sync(&s.key, &s.project, &s.path).unwrap() {}
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let results = store.history(Some(&project), 20, 0).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|s| s.project == sub && s.count == 1));
    assert!(results.iter().any(|s| Some(&s.key) == original.as_ref()));
    assert_eq!(results[0].key, ag);
    catalog.request(project.clone(), &roots);
    assert!(!catalog.scanning());
    let late = key("codex", 9999);
    let path = source(&roots, &late);
    put(
        &path,
        serde_json::json!({"type":"session_meta","payload":{"id":late.id,"cwd":sub}}),
    );
    put(
        &path,
        prompt(&late, 0, "外部刚提交", Some("2026-11-22T04:12:00Z")),
    );
    catalog.refresh(&roots);
    let deadline = Instant::now() + Duration::from_secs(12);
    while catalog.scanning() {
        catalog.poll();
        while let Some(s) = catalog.pending.pop_front() {
            while store.sync(&s.key, &s.project, &s.path).unwrap() {}
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(store.history(Some(&project), 20, 0).unwrap().len(), 3);
    assert_eq!(store.get(&late).unwrap().unwrap().count, 1);
    catalog.request(other, &roots);
    assert_eq!(catalog.pending.len(), 105);
}

#[test]
fn zero_animation_and_same_frame_terminal_interaction() {
    use std::rc::Rc;

    let (dir, _, _) = index_fixture("zero-animation");
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    rt.update(|w, _| w.first_open = false);
    let clock = Instant::now();
    let live = super::multiple::launch_profile_shell(&mut rt, &dir);
    wait_for_workspace(&mut rt, clock, |w| w.focused && !w.screen.rows.is_empty());

    // Populate enough projects so `project-scroll` overflows and can scroll.
    let mut extra_ids = Vec::new();
    rt.update(|w, _| {
        for n in 0..12 {
            let id = SessionId::new();
            let mut tab = crate::workspace::Tab::pending(
                id,
                dir.join(format!("proj-{n:02}")),
                std::sync::mpsc::channel().1,
            );
            tab.opening = None;
            w.tabs.push(tab);
            extra_ids.push(id);
        }
    });
    rt.frame(0.016);
    rt.frame(0.016);

    // 1. Chinese IME Preedit -> Commit must land in 1 frame with zero lingering animation.
    rt.ime(Ime::Preedit("中文输入".into(), None, Vec::new()));
    assert!(!rt.frame(0.016), "IME preedit 不应启动动画");
    assert_eq!(rt.state.preedit, "中文输入");
    rt.ime(Ime::Commit("中文".into()));
    assert!(!rt.frame(0.016), "IME commit 不应启动动画或淡出遮挡");
    assert!(rt.state.preedit.is_empty());
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).contains("中文")
    });
    assert!(!rt.frame(0.016));

    // 2. Idle pointer movement over terminal must be live_only (zero view commits).
    let [tx, ty, tw, th] = rt.presented("terminal").unwrap().0;
    let commits_before = rt.stats.commits;
    let live_only_before = rt.live_only_events();
    for step in 1..=6 {
        rt.pointer_move(tx + 12. + step as f32 * 8., ty + 12.);
        assert!(!rt.frame(0.016), "终端内悬停移动不应启动动画");
    }
    assert_eq!(
        rt.stats.commits, commits_before,
        "终端内非拖拽指针移动不应触发视图重算"
    );
    assert!(
        rt.live_only_events() >= live_only_before + 6,
        "终端内指针移动应走 live_only 路径"
    );

    // 3. Terminal drag selection updates rows on the same frame and only rebuilds touched rows.
    let rows_before: Vec<_> = rt.state.screen.rows.clone();
    rt.state
        .session()
        .unwrap()
        .listener
        .dirty
        .store(false, std::sync::atomic::Ordering::Release);
    let drag_y = ty + crate::terminal_view::HEIGHT * 0.5;
    rt.pointer_down(tx + 5., drag_y);
    rt.pointer_move(tx + rt.state.cell * 6., drag_y);
    assert!(!rt.frame(0.016), "终端拖选不应启动动画");
    assert!(
        !rt.state
            .session()
            .unwrap()
            .listener
            .dirty
            .load(std::sync::atomic::Ordering::Acquire),
        "同帧已刷新的拖选不应再次置位 dirty 触发多余唤醒"
    );
    assert!(
        rt.state
            .screen
            .selection_fills
            .iter()
            .any(|&(row, c0, c1, _)| row == 0 && c1 > c0),
        "拖选首行应在当帧生成选区叠层"
    );
    assert!(
        Rc::ptr_eq(&rows_before[0], &rt.state.screen.rows[0]),
        "拖选应复用原文本行 Rc<GridRow> 而不触发字形重排"
    );
    let rebuilt_rows = rows_before
        .iter()
        .zip(&rt.state.screen.rows)
        .filter(|(a, b)| !Rc::ptr_eq(a, b))
        .count();
    assert!(
        rebuilt_rows <= 1,
        "拖选单行不应重建文本行：实际重建 {rebuilt_rows}/{} 行",
        rows_before.len()
    );
    let commits_mid = rt.stats.commits;
    // Sub-cell movement within the same cell/side must skip redundant commits.
    rt.pointer_move(tx + rt.state.cell * 6. + 0.1, drag_y);
    assert!(!rt.frame(0.016));
    assert_eq!(
        rt.stats.commits, commits_mid,
        "同一字符单元格内的微移不应触发额外重算"
    );
    rt.pointer_up(tx + rt.state.cell * 6., drag_y);
    assert!(!rt.frame(0.016), "松开选区不应启动动画");
    assert!(tw > 0. && th > 0.);

    // 4. Left tree wheel scroll lands on frame 1 with zero glide animation.
    let [sx, sy, sw, sh] = rt.presented("project-scroll").unwrap().0;
    let first_proj_key = format!("project-{}", live.0);
    let y_before = rt.presented(&first_proj_key).unwrap().0[1];
    rt.wheel(sx + sw * 0.5, sy + sh * 0.5, -60.);
    assert!(!rt.frame(0.016), "左侧项目树滚轮不应存在 200ms 缓动滑行");
    let y_after = rt.presented(&first_proj_key).unwrap().0[1];
    assert!(y_after < y_before, "左侧项目树滚轮应在首帧立即位移");
    rt.wheel(sx + sw * 0.5, sy + sh * 0.5, 120.);
    assert!(!rt.frame(0.016));

    // 5. Collapsing a project group and toggling archive/footprints land in 1 frame with zero animation.
    let next_proj_key = format!("project-{}", extra_ids[0].0);
    let next_y_open = rt.presented(&next_proj_key).unwrap().0[1];
    let [px, py, pw, ph] = rt.presented(&first_proj_key).unwrap().0;
    rt.pointer_down(px + pw * 0.5, py + ph * 0.5);
    rt.pointer_up(px + pw * 0.5, py + ph * 0.5);
    assert!(!rt.frame(0.016), "项目折叠不应启动弹簧动画");
    let next_y_closed_1 = rt.presented(&next_proj_key).unwrap().0[1];
    assert!(next_y_closed_1 < next_y_open);
    assert!(!rt.frame(0.016));
    assert_eq!(rt.presented(&next_proj_key).unwrap().0[1], next_y_closed_1);

    let [hx, hy, hw, hh] = rt.presented("history-toggle").unwrap().0;
    rt.pointer_down(hx + hw * 0.5, hy + hh * 0.5);
    rt.pointer_up(hx + hw * 0.5, hy + hh * 0.5);
    rt.frame(0.016);
    let recent_open_rect = rt.presented("recent-tasks").unwrap().0;
    assert!(rt.presented("recent-scroll").is_some());
    assert!(!rt.frame(0.016), "展开全部历史/最近任务区不应启动动画");
    assert_eq!(rt.presented("recent-tasks").unwrap().0, recent_open_rect);

    let [rx, ry, rw, rh] = rt.presented("recent-toggle").unwrap().0;
    rt.pointer_down(rx + rw * 0.5, ry + rh * 0.5);
    rt.pointer_up(rx + rw * 0.5, ry + rh * 0.5);
    rt.frame(0.016);
    let recent_closed_rect = rt.presented("recent-tasks").unwrap().0;
    assert!(rt.presented("recent-scroll").is_none());
    assert!(!rt.frame(0.016), "收起最近任务区不应启动动画");
    assert_eq!(rt.presented("recent-tasks").unwrap().0, recent_closed_rect);

    // 6. Splitter drag follows the pointer on frame 1 without animation or redundant layout-echo commits.
    let [dx, dy, dw, dh] = rt.presented("sidebar-divider").unwrap().0;
    let (cx, cy) = (dx + dw * 0.5, dy + dh * 0.5);
    rt.pointer_down(cx, cy);
    rt.pointer_move(cx + 60., cy);
    rt.frame(0.016);
    let moved_cx = {
        let [nx, _, nw, _] = rt.presented("sidebar-divider").unwrap().0;
        nx + nw * 0.5
    };
    assert!(
        (moved_cx - (cx + 60.)).abs() <= 1.,
        "分隔线应在首帧紧跟指针：期望 {}，实际 {moved_cx}",
        cx + 60.
    );
    // Let focus transition from terminal to divider settle before measuring pure drag layout-echo.
    assert!(!rt.frame(0.016), "拖动分隔线不应启动动画");
    rt.pointer_move(cx + 90., cy);
    rt.frame(0.016);
    let commits_after_drag = rt.stats.commits;
    let live_only_after_drag = rt.live_only_events();
    assert!(!rt.frame(0.016), "持续拖动分隔线不应启动动画");
    assert_eq!(
        rt.stats.commits, commits_after_drag,
        "拖动中的 on_layout 回报不应触发多余的整树重算"
    );
    assert!(
        rt.live_only_events() > live_only_after_drag,
        "拖动中的 on_layout 回报应走 live_only 路径"
    );
    rt.pointer_up(cx + 90., cy);
    assert!(!rt.frame(0.016), "松开分隔线不应启动动画");

    // 7. Footprints drawer floats over the terminal even on wide viewports without resizing ConPTY.
    rt.set_viewport((1440., 720.), 1.);
    rt.frame(0.016);
    rt.frame(0.016);
    let cols_before_drawer = rt.state.size.cols;
    let term_rect_before = rt.presented("terminal").unwrap().0;
    let [fx, fy, fw, fh] = rt.presented("footprint-toggle").unwrap().0;
    rt.pointer_down(fx + fw * 0.5, fy + fh * 0.5);
    rt.pointer_up(fx + fw * 0.5, fy + fh * 0.5);
    assert!(!rt.frame(0.016), "打开提示词足迹浮层不应启动动画");
    assert!(rt.presented("footprints").is_some());
    assert_eq!(
        rt.state.size.cols, cols_before_drawer,
        "宽屏下打开浮层抽屉不应触发终端列数变化"
    );
    assert_eq!(rt.presented("terminal").unwrap().0, term_rect_before);
    let [clx, cly, clw, clh] = rt.presented("close-footprints").unwrap().0;
    rt.pointer_down(clx + clw * 0.5, cly + clh * 0.5);
    rt.pointer_up(clx + clw * 0.5, cly + clh * 0.5);
    rt.frame(0.016);
    assert!(rt.presented("footprints").is_none());
    assert_eq!(rt.state.size.cols, cols_before_drawer);
    assert_eq!(rt.presented("terminal").unwrap().0, term_rect_before);
    assert!(!rt.frame(0.016), "关闭提示词足迹浮层不应启动动画");
}

#[test]
fn settings_close_to_tray_and_rebind_shortcuts_persist() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/settings-tests")
        .join(format!("{}-{}", std::process::id(), SessionId::new().0));
    std::fs::create_dir_all(&dir).unwrap();
    let prefs_path = dir.join("preferences.json");

    let mut rt = app().headless_runtime((1180., 680.), rune::Env::new(true, 1.));
    let (live, second) = (SessionId::new(), SessionId::new());
    rt.update(|w, tx| {
        w.first_open = false;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path.clone()));
        let mut t1 = crate::workspace::Tab::pending(live, dir.join("alpha"), mpsc::channel().1);
        t1.opening = None;
        let mut t2 = crate::workspace::Tab::pending(second, dir.join("beta"), mpsc::channel().1);
        t2.opening = None;
        w.tabs.push(t1);
        w.tabs.push(t2);
        w.select(Some(live), tx);
    });
    rt.frame(0.016);

    // 1. Open Settings page and enable Close-to-Tray setting.
    let [nx, ny, nw, nh] = rt.presented("nav-settings").unwrap().0;
    rt.pointer_down(nx + nw * 0.5, ny + nh * 0.5);
    rt.pointer_up(nx + nw * 0.5, ny + nh * 0.5);
    rt.frame(0.016);
    assert!(rt.state.layout.settings);
    assert!(!rt.state.layout.close_to_tray);
    rt.update(|w, tx| w.on_close_window(tx));
    assert!(
        rt.shell_request.take().is_some(),
        "未开启最小化到托盘时，关闭窗口应退出应用"
    );
    let [tx_x, tx_y, tx_w, tx_h] = rt.presented("setting-close-to-tray").unwrap().0;
    rt.pointer_down(tx_x + tx_w * 0.5, tx_y + tx_h * 0.5);
    rt.pointer_up(tx_x + tx_w * 0.5, tx_y + tx_h * 0.5);
    rt.frame(0.016);
    assert!(rt.state.layout.close_to_tray);
    rt.update(|w, tx| w.on_close_window(tx));
    assert!(
        rt.shell_request.take().is_none(),
        "开启最小化到托盘后，关闭窗口应保留后台运行而非退出"
    );

    // 1b. Toggle top system metrics bar off and verify it disappears.
    assert!(rt.state.layout.show_metrics_bar);
    assert!(rt.presented("system-metrics").is_some());
    let [mx, my, mw, mh] = rt.presented("setting-metrics-bar").unwrap().0;
    rt.pointer_down(mx + mw * 0.5, my + mh * 0.5);
    rt.pointer_up(mx + mw * 0.5, my + mh * 0.5);
    rt.frame(0.016);
    assert!(!rt.state.layout.show_metrics_bar);
    assert!(rt.presented("system-metrics").is_none());

    // 2. Rebind Footprints shortcut (default Ctrl+Shift+H -> Alt+H) with validation checks.
    let [sx, sy, sw, sh] = rt.presented("shortcut-footprints").unwrap().0;
    rt.pointer_down(sx + sw * 0.5, sy + sh * 0.5);
    rt.pointer_up(sx + sw * 0.5, sy + sh * 0.5);
    rt.frame(0.016);
    assert_eq!(
        rt.state.layout.recording_shortcut,
        Some(crate::workbench::ShortcutAction::Footprints)
    );

    let ev = |key: Key, mods: Mods| KeyEvent { key, mods };

    // Bare key rejected
    rt.key(ev(Key::Text("h".into()), Mods::default()));
    rt.frame(0.016);
    assert!(rt.state.layout.shortcut_notice.is_some());
    assert!(rt.state.layout.recording_shortcut.is_some());

    // Reserved Ctrl+C rejected
    rt.key(ev(
        Key::Command('c'),
        Mods {
            ctrl: true,
            ..Default::default()
        },
    ));
    rt.frame(0.016);
    assert!(
        rt.state
            .layout
            .shortcut_notice
            .as_deref()
            .is_some_and(|m| m.contains("Ctrl+C"))
    );

    // Conflict with NewShell (Ctrl+Shift+T) rejected
    rt.key(ev(
        Key::Command('t'),
        Mods {
            ctrl: true,
            shift: true,
            ..Default::default()
        },
    ));
    rt.frame(0.016);
    assert!(
        rt.state
            .layout
            .shortcut_notice
            .as_deref()
            .is_some_and(|m| m.contains("新建终端"))
    );

    // Valid Alt+H accepted and persisted
    rt.key(ev(
        Key::Text("h".into()),
        Mods {
            alt: true,
            ..Default::default()
        },
    ));
    rt.frame(0.016);
    assert_eq!(rt.state.layout.recording_shortcut, None);
    assert_eq!(
        rt.state
            .layout
            .shortcuts
            .get(crate::workbench::ShortcutAction::Footprints)
            .display(),
        "Alt+H"
    );

    // 3. Verify old Ctrl+Shift+H no longer toggles footprints, while new Alt+H does.
    rt.key(ev(
        Key::Command('h'),
        Mods {
            ctrl: true,
            shift: true,
            ..Default::default()
        },
    ));
    rt.frame(0.016);
    assert!(rt.presented("footprints").is_none());

    rt.key(ev(
        Key::Text("h".into()),
        Mods {
            alt: true,
            ..Default::default()
        },
    ));
    rt.frame(0.016);
    assert!(rt.presented("footprints").is_some());
    rt.key(ev(
        Key::Text("h".into()),
        Mods {
            alt: true,
            ..Default::default()
        },
    ));
    rt.frame(0.016);
    assert!(rt.presented("footprints").is_none());

    // 4. Flush preference writer and verify persistence across restart.
    rt.update(|w, _| w.layout.finish_width_write());
    let reloaded = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path.clone()));
    assert!(reloaded.close_to_tray);
    assert!(!reloaded.show_metrics_bar);
    assert_eq!(
        reloaded
            .shortcuts
            .get(crate::workbench::ShortcutAction::Footprints)
            .display(),
        "Alt+H"
    );

    // 5. Reset shortcuts to defaults via UI button.
    let [rx, ry, rw, rh] = rt.presented("setting-shortcuts-reset").unwrap().0;
    rt.pointer_down(rx + rw * 0.5, ry + rh * 0.5);
    rt.pointer_up(rx + rw * 0.5, ry + rh * 0.5);
    rt.frame(0.016);
    assert!(rt.state.layout.shortcuts.is_default());
    rt.update(|w, _| w.layout.finish_width_write());
    let reloaded_reset = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path));
    assert!(reloaded_reset.shortcuts.is_default());
}

#[test]
fn normalize_explorer_path_handles_verbatim_unc_and_trailing_slashes() {
    use crate::sidebar::normalize_explorer_path;
    assert_eq!(
        normalize_explorer_path(Path::new(r"\\?\L:\AI\Mcli\")),
        PathBuf::from(r"L:\AI\Mcli")
    );
    assert_eq!(
        normalize_explorer_path(Path::new(r"\\?\UNC\fileserver\share\proj\")),
        PathBuf::from(r"\\fileserver\share\proj")
    );
    assert_eq!(
        normalize_explorer_path(Path::new("L:/AI/My App/")),
        PathBuf::from(r"L:\AI\My App")
    );
    assert_eq!(
        normalize_explorer_path(Path::new(r"C:\")),
        PathBuf::from(r"C:\")
    );
}

#[test]
fn project_context_menu_expands_and_collapses_all_directories() {
    let mut rt = app().headless_runtime((1040., 680.), rune::Env::new(true, 1.));
    let id_a = SessionId::new();
    let id_b = SessionId::new();
    rt.update(|w, tx| {
        w.first_open = false;
        for (id, dir) in [(id_a, r"C:\proj-alpha"), (id_b, r"C:\proj-beta")] {
            let mut tab = crate::workspace::Tab::pending(
                id,
                PathBuf::from(dir),
                std::sync::mpsc::channel().1,
            );
            tab.opening = None;
            w.tabs.push(tab);
        }
        w.select(Some(id_a), tx);
    });
    rt.frame(0.1);
    let anchor = format!("project-{}", id_a.0);
    let [x, y, w, h] = rt.presented(&anchor).unwrap().0;
    rt.secondary_down(x + w * 0.5, y + h * 0.5);
    rt.secondary_up(x + w * 0.5, y + h * 0.5);
    rt.frame(0.12);
    assert!(rt.presented("mcli-context-menu").is_some());
    assert!(rt.presented("menu-item-open-dir").is_some());
    assert!(rt.presented("menu-item-new-shell").is_some());
    assert!(rt.presented("menu-item-copy-path").is_some());
    assert!(rt.presented("menu-item-expand-all").is_some());
    assert!(rt.presented("menu-item-collapse-all").is_some());

    // Collapse all projects via Mcli styled context menu.
    press(&mut rt, "menu-item-collapse-all", 0.14);
    rt.frame(0.15);
    assert!(rt.presented("mcli-context-menu").is_none());
    assert_eq!(rt.state.layout.collapsed.len(), 2);
    assert!(rt.presented(&format!("session-{}", id_a.0)).is_none());
    assert!(rt.presented(&format!("session-{}", id_b.0)).is_none());

    // Re-open context menu and expand all projects.
    let [x, y, w, h] = rt.presented(&anchor).unwrap().0;
    rt.secondary_down(x + w * 0.5, y + h * 0.5);
    rt.secondary_up(x + w * 0.5, y + h * 0.5);
    rt.frame(0.16);
    press(&mut rt, "menu-item-expand-all", 0.18);
    rt.frame(0.19);
    assert!(rt.state.layout.collapsed.is_empty());
    assert!(rt.presented(&format!("session-{}", id_a.0)).is_some());
    assert!(rt.presented(&format!("session-{}", id_b.0)).is_some());
}

#[test]
fn proxy_profiles_settings_and_top_bar_switcher() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/proxy-ui-tests")
        .join(format!("{}-{}", std::process::id(), SessionId::new().0));
    std::fs::create_dir_all(&dir).unwrap();
    let prefs_path = dir.join("preferences.json");

    let mut rt = app().headless_runtime((1180., 680.), rune::Env::new(true, 1.));
    let live = SessionId::new();
    rt.update(|w, tx| {
        w.first_open = false;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path.clone()));
        let mut t1 = crate::workspace::Tab::pending(live, dir.clone(), mpsc::channel().1);
        t1.opening = None;
        w.tabs.push(t1);
        w.select(Some(live), tx);
    });
    rt.frame(0.016);

    // 1. Add two profiles (port shorthand + full URL) and verify persistence & catalog sync.
    rt.update(|w, tx| {
        w.layout.proxy_name_input = rune::TextEdit::new("p1");
        w.layout.proxy_url_input = rune::TextEdit::new("10808");
        w.add_proxy_profile(tx);
        w.layout.proxy_name_input = rune::TextEdit::new("p2");
        w.layout.proxy_url_input = rune::TextEdit::new("http://127.0.0.1:7890");
        w.add_proxy_profile(tx);
        w.set_default_proxy_profile(Some("p1".into()), tx);
        w.set_agent_default_proxy("codex", Some("p2".into()), tx);
        w.layout.finish_width_write();
    });
    rt.frame(0.032);
    assert_eq!(rt.state.layout.proxy.profiles.len(), 2);
    assert!(dir.join("proxy-profiles.json").is_file());
    let reloaded = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path));
    assert_eq!(reloaded.proxy.profiles.len(), 2);
    assert_eq!(reloaded.proxy.profiles[0].url, "http://127.0.0.1:10808");
    assert_eq!(reloaded.proxy.default_profile.as_deref(), Some("p1"));
    assert_eq!(reloaded.proxy.agent_default("codex"), Some("p2"));

    // 2. Click top-bar proxy switcher pill and pick p1, then manage.
    press(&mut rt, "proxy-switcher", 0.05);
    rt.frame(0.06);
    assert!(rt.presented("mcli-context-menu").is_some());
    assert!(rt.presented("menu-item-sys").is_some());
    assert!(rt.presented("menu-item-off").is_some());
    assert!(rt.presented("menu-item-profile-p1").is_some());
    assert!(rt.presented("menu-item-profile-p2").is_some());
    assert!(rt.presented("menu-item-manage").is_some());

    press(&mut rt, "menu-item-profile-p1", 0.07);
    rt.frame(0.08);
    assert!(rt.presented("mcli-context-menu").is_none());
    assert_eq!(rt.state.tabs[0].proxy_summary, "http://127.0.0.1:10808");

    press(&mut rt, "proxy-switcher", 0.09);
    rt.frame(0.10);
    press(&mut rt, "menu-item-manage", 0.11);
    rt.frame(0.12);
    assert!(rt.state.layout.settings);
    assert!(rt.presented("mcli-context-menu").is_none());
}

#[test]
fn agent_completion_sound_respects_background_mode_waiting_and_persists() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/sound-tests")
        .join(format!("{}-{}", std::process::id(), SessionId::new().0));
    std::fs::create_dir_all(&dir).unwrap();
    let prefs_path = dir.join("preferences.json");

    let mut rt = app().headless_runtime((1180., 680.), rune::Env::new(true, 1.));
    let (id_a, id_b) = (SessionId::new(), SessionId::new());
    rt.update(|w, tx| {
        w.first_open = false;
        w.window_active = true;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path.clone()));
        for (id, name) in [(id_a, "alpha"), (id_b, "beta")] {
            let mut tab = crate::workspace::Tab::pending(id, dir.join(name), mpsc::channel().1);
            tab.opening = None;
            tab.foreground = Some(crate::shell::Foreground::Agy);
            tab.phase = "运行中".into();
            w.tabs.push(tab);
        }
        w.select(Some(id_a), tx);
        // Prime `was_running` on both tabs.
        let _ = w.tabs[0].observe_reply(true);
        let _ = w.tabs[1].observe_reply(false);
    });
    let _ = crate::sound::take_test_events();

    // 1. Active window + selected tab completes -> silent in default Background mode.
    rt.update(|w, _| {
        w.tabs[0].phase = "就绪".into();
        if let Some(kind) = w.tabs[0].observe_reply(true) {
            w.notify_agent_sound(kind, true);
        }
    });
    assert!(crate::sound::take_test_events().is_empty());

    // 2. Non-selected background tab completes -> plays Terminal sound.
    rt.update(|w, _| {
        w.tabs[1].phase = "就绪".into();
        if let Some(kind) = w.tabs[1].observe_reply(false) {
            w.notify_agent_sound(kind, false);
        }
    });
    assert_eq!(
        crate::sound::take_test_events(),
        vec![crate::sound::SoundPreset::Terminal]
    );

    // 3. Window unfocused + selected tab enters Waiting -> plays sound.
    rt.update(|w, _| {
        w.window_active = false;
        w.tabs[0].phase = "等待用户".into();
        if let Some(kind) = w.tabs[0].observe_reply(true) {
            w.notify_agent_sound(kind, true);
        }
    });
    assert_eq!(
        crate::sound::take_test_events(),
        vec![crate::sound::SoundPreset::Terminal]
    );

    // 4. Persist sound preferences across reload.
    rt.update(|w, tx| {
        w.set_sound_mode(crate::sound::SoundMode::Always, tx);
        w.set_sound_preset(crate::sound::SoundPreset::Notify, tx);
        w.set_sound_on_waiting(false, tx);
        w.layout.finish_width_write();
    });
    assert_eq!(
        crate::sound::take_test_events(),
        vec![crate::sound::SoundPreset::Notify]
    );
    let reloaded = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path));
    assert_eq!(reloaded.sound.mode, crate::sound::SoundMode::Always);
    assert_eq!(reloaded.sound.preset, crate::sound::SoundPreset::Notify);
    assert!(!reloaded.sound.on_waiting);
}
