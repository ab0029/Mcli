use super::conversation_index::{fixture, key, prompt, put, source};
use super::*;
use crate::history::Service;
use crate::hook_wire::{Notice, host, pipe_name};
use crate::transcript::Key as Conversation;
use serde_json::json;
use std::io::Write;

fn event(id: SessionId, run: &str, k: &Conversation, path: &Path, event: &str, at: u64) {
    let marker = id.marker();
    let payload = json!({"conversationId":k.id,"transcriptPath":path,"lastUserInput":"第一行\n  中文","terminationReason":"model_stop"});
    let mut n = Notice::from_hook("agy", event, marker.clone(), run.into(), &payload).unwrap();
    n.sent = at;
    let pipe = pipe_name(&host(&marker).unwrap().1);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&pipe) {
            f.write_all(&n.encode()).unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(15));
    }
}
pub(super) fn shown(rt: &mut rune::Runtime<Workspace>, clock: Instant, control: &str) {
    let until = Instant::now() + Duration::from_secs(5);
    while rt.presented(control).is_none() {
        rt.frame(clock.elapsed().as_secs_f64());
        assert!(Instant::now() < until, "控件未出现 {control}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn agy_native_ready_expires_at_the_next_accepted_prompt() {
    use crate::history::{Command, Update};
    use crate::shell::AgyEvent;
    let (dir, roots, db) = fixture("agy-ready-order");
    let key = key("agy", 1);
    let path = source(&roots, &key);
    let reply =
        |text| json!({"type":"PLANNER_RESPONSE","source":"MODEL","status":"DONE","content":text});
    put(
        &path,
        prompt(&key, 0, "first", Some("1970-01-01T00:00:01Z")),
    );
    put(&path, reply("1517"));
    let service = Service::start(db, roots, Arc::new(|| {}));
    let marker = SessionId::new().marker();
    let run = "00112233445566778899aabbccddeeff";
    service
        .send
        .send(Command::Foreground {
            marker: marker.clone(),
            run: run.into(),
            agent: "agy".into(),
            project: dir,
            restore: None,
            initial: None,
            proxy: None,
        })
        .unwrap();
    let send = |run: &str, sent, event| {
        service
            .send
            .send(Command::Agy(marker.clone(), run.into(), sent, event))
            .unwrap();
    };
    let bound = |phase: &str, preview: &str, count| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let update = service
                .receive
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if let Update::Bound(_, _, summary) = update {
                assert_eq!(summary.key, key);
                assert_eq!(
                    (
                        summary.phase.as_str(),
                        summary.preview.as_str(),
                        summary.count
                    ),
                    (phase, preview, count)
                );
                return;
            }
        }
    };
    send(run, 1_000_000, AgyEvent::Thread(Some(key.id.clone())));
    bound("运行中", "first", 1);
    let vt = std::env::var_os("MCLI_QA_AGY_READY_REPLAY")
        .map(|file| std::fs::read(file).unwrap())
        .unwrap_or_else(|| b"  1517\r\n\x1b]9;Antigravity CLI is ready for input\x07".to_vec());
    let mut parser = shell::ShellReports::default();
    for bytes in vt.chunks(7) {
        parser.feed(bytes, &marker, |report| {
            if let shell::Report::Status(Some(shell::Foreground::Agy), state, _) = report {
                send(run, 2_000_000, AgyEvent::Status(state));
            }
        });
    }
    bound("就绪", "1517", 1);

    put(
        &path,
        prompt(&key, 2, "second", Some("1970-01-01T00:00:03Z")),
    );
    put(&path, reply("intermediate text"));
    send(run, 1_500_000, AgyEvent::Status("运行中"));
    send("another-run", 5_000_000, AgyEvent::Status("就绪"));
    bound("运行中", "second", 2);
    send(run, 4_000_000, AgyEvent::Status("等待用户"));
    bound("等待用户", "second", 2);
    send(run, 3_500_000, AgyEvent::Status("就绪"));
    send(run, 5_000_000, AgyEvent::Status("运行中"));
    bound("运行中", "second", 2);
    send(run, 6_000_000, AgyEvent::Status("就绪"));
    bound("就绪", "intermediate text", 2);
    // The provider may flush its final text after reporting ready.
    put(&path, reply("second reply"));
    bound("就绪", "second reply", 2);
}

#[test]
fn history_restore_reports_a_missing_project_or_cli_and_keeps_a_working_shell() {
    let (dir, _, _) = fixture("restore-failure");
    let missing = dir.join("已移动的项目");
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1120., 720.), rune::Env::new(true, 1.));
    rt.update(|w, _| w.first_open = false);
    let original = super::multiple::launch_profile_shell(&mut rt, &dir);
    wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    rt.ime(rune::Ime::Commit("Q7".into()));
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).replace('\n', "").contains("Q7")
    });
    rt.update(|w, tx| w.open_shell(Some(missing.clone()), Some(key("codex", 1)), tx));
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().is_some_and(|t| t.error.is_some())
    });
    let failed = rt.state.current().unwrap();
    assert_ne!(failed.id, original);
    assert!(failed.session.is_none() && failed.bound.is_none());
    assert!(
        failed
            .error
            .as_deref()
            .unwrap()
            .contains(missing.to_str().unwrap()),
        "应指出无法打开的历史目录"
    );
    press(
        &mut rt,
        &format!("session-{}", original.0),
        clock.elapsed().as_secs_f64(),
    );
    assert!(
        frame(rt.state.session().unwrap())
            .replace('\n', "")
            .contains("Q7")
    );
    drop(rt);

    let shell = shell::program().unwrap();
    for vendor in ["codex", "agy"] {
        let mut args = shell::arguments_for(Some(&key(vendor, 9)));
        args.insert(0, "-NoProfile".into());
        args.last_mut().unwrap().insert_str(0, "$env:PATH = ''\n");
        let session =
            Session::spawn(&shell, &args, &dir, Size::default(), Arc::new(|| {})).unwrap();
        wait_for(&session, 12, || {
            session.listener.shell.lock().unwrap().restore_end
                == Some(shell::RestoreEnd::Unavailable)
        });
        assert!(
            frame(&session).contains(&format!("找不到 {vendor}")),
            "应指出缺少的 CLI {vendor}"
        );
        session.input(b"[Console]::WriteLine(('SHELL-OK-{0}' -f 42))\r".to_vec());
        wait_for(&session, 6, || frame(&session).contains("SHELL-OK-42"));
    }
}

#[test]
fn opening_footprints_preserves_a_selection_in_scrollback() {
    let (session, dir) = super::fixture(
        "drawer-scrollback",
        r#"1..100 | ForEach-Object { [Console]::WriteLine(("LOG-{0:D3}" -f $_)) }
[Console]::WriteLine("READY"); $null=[Console]::ReadLine()"#,
        &[],
    );
    wait_for(&session, 10, || frame(&session).contains("READY"));
    let mut rt = app().headless_runtime((1120., 720.), rune::Env::new(true, 1.0));
    rt.update(|w, tx| {
        w.first_open = false;
        attach_session(w, session, &dir, tx);
    });
    let clock = Instant::now();
    wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    let s = rt.state.session().unwrap();
    s.scroll(8);
    s.select(0, 1, false, Some(SelectionType::Simple));
    s.select(6, 1, true, None);
    let selected = s.selected().expect("选择历史行的文字");
    let offset = s.term.lock().unwrap().grid().display_offset();
    let columns = rt.state.size.cols;
    assert!(offset > 0 && selected.starts_with("LOG-"));
    for key in ["footprint-toggle", "close-footprints"] {
        press(&mut rt, key, clock.elapsed().as_secs_f64());
        for _ in 0..4 {
            rt.frame(0.016);
        }
        let s = rt.state.session().unwrap();
        assert_eq!(
            s.selected().as_ref(),
            Some(&selected),
            "{key} 改变了选区，网格 {columns} → {}，窗口 {}，终端 {:?}",
            rt.state.size.cols,
            rt.state.layout.viewport.get()[0],
            rt.presented("terminal")
        );
        assert_eq!(s.term.lock().unwrap().grid().display_offset(), offset);
        assert_eq!(rt.state.size.cols, columns);
    }
}
#[test]
fn recent_drawer_copy_focus_and_late_old_stop_use_the_bound_terminal() {
    let (dir, roots, db) = fixture("rune");
    drop(crate::conversation_store::Store::open(&db, roots.clone()).unwrap());
    rusqlite::Connection::open(&db).unwrap().execute(
        "INSERT INTO conversations(agent,id,project,source,prompt_count) VALUES('codex','invalid-id',?,'',1)",
        [dir.to_str().unwrap()],
    ).unwrap();
    let a1 = key("agy", 1);
    let a2 = key("agy", 2);
    let file = source(&roots, &a1);
    let other = source(&roots, &a2);
    put(
        &file,
        prompt(&a1, 0, "相同提问", Some("2026-11-22T02:00:00Z")),
    );
    put(
        &file,
        prompt(&a1, 1, "第一行\n  中文", Some("2026-11-22T02:00:02Z")),
    );
    put(
        &file,
        json!({"type":"PLANNER_RESPONSE","source":"MODEL","status":"DONE","content":"答复 A1"}),
    );
    put(
        &other,
        prompt(&a2, 0, "A2 独有", Some("2026-11-22T02:01:00Z")),
    );
    let exe = dir.join("agy.ps1");
    std::fs::write(
        &exe,
        r#"
[Console]::WriteLine("SYNTHETIC-CLI")
$first = [Console]::ReadLine()
$second = [Console]::ReadLine()
[Console]::WriteLine("PASTED<$first><$second>")
$null = [Console]::ReadLine()
"#,
    )
    .unwrap();
    let mut rt = app().headless_runtime((1120., 720.), rune::Env::new(true, 1.0));
    rt.update(|w, _| {
        w.first_open = false;
        w.history.service = Some(Service::start(db.clone(), roots.clone(), w.wake.clone()));
    });
    let clock = Instant::now();
    let board = rune::clipboard::memory();
    rt.use_clipboard(&board);
    let id = super::multiple::launch_profile_shell(&mut rt, &dir);
    shown(&mut rt, clock, "task-codex-invalid-id");
    press(
        &mut rt,
        "task-codex-invalid-id",
        clock.elapsed().as_secs_f64(),
    );
    assert!(rt.state.history.error.contains("编号无效"));
    assert_eq!(rt.state.active, Some(id));
    assert_eq!(rt.state.tabs.len(), 1);
    press(
        &mut rt,
        &format!("session-{}", id.0),
        clock.elapsed().as_secs_f64(),
    );
    shell_line(&mut rt, &format!("& {}", shell_quote(&exe)));
    wait_for_workspace(&mut rt, clock, |w| w.current().unwrap().run.is_some());
    let run = rt.state.current().unwrap().run.clone().unwrap();
    event(id, &run, &a1, &file, "SessionStart", 1);
    wait_for_workspace(&mut rt, clock, |w| w.current().unwrap().prompt_count == 2);
    event(id, &run, &a1, &file, "Stop", 2);
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().unwrap().preview == "答复 A1"
    });
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).contains("SYNTHETIC-CLI")
    });
    rt.key(KeyEvent {
        key: Key::Text("DRAFT-KEPT".into()),
        mods: Mods::default(),
    });
    press(&mut rt, "footprint-toggle", clock.elapsed().as_secs_f64());
    shown(&mut rt, clock, "copy-prompt-2");
    rt.update(|_, tx| tx.focus("prompt-search", true));
    rt.frame(clock.elapsed().as_secs_f64());
    rt.ime(Ime::Commit("  中文".into()));
    let until = Instant::now() + Duration::from_secs(6);
    loop {
        rt.frame(clock.elapsed().as_secs_f64());
        if rt.presented("copy-prompt-2").is_some() && rt.presented("copy-prompt-1").is_none() {
            break;
        }
        assert!(Instant::now() < until, "跨行原文搜索应只命中第二条提问");
        std::thread::sleep(Duration::from_millis(15));
    }
    press(&mut rt, "copy-prompt-2", clock.elapsed().as_secs_f64());
    let deadline = Instant::now() + Duration::from_secs(5);
    while board.text().as_deref().map(|s| s.as_str()) != Some("第一行\n  中文") {
        rt.frame(clock.elapsed().as_secs_f64());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(15));
    }
    press(&mut rt, "close-footprints", clock.elapsed().as_secs_f64());
    let item = format!("task-agy-{}", a1.id);
    shown(&mut rt, clock, &item);
    press(&mut rt, &item, clock.elapsed().as_secs_f64());
    assert_eq!(rt.state.active, Some(id));
    assert_eq!(rt.state.tabs.len(), 1);
    assert!(
        rt.state.history.error.is_empty(),
        "成功切回对话后仍显示上一次恢复失败"
    );
    assert!(frame(rt.state.session().unwrap()).contains("DRAFT-KEPT"));
    rt.key(KeyEvent {
        key: Key::Command('v'),
        mods: Mods {
            ctrl: true,
            ..Default::default()
        },
    });
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).contains("  中文")
    });
    rt.key(KeyEvent {
        key: Key::Enter,
        mods: Mods::default(),
    });
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).contains("PASTED<DRAFT-KEPT第一行><  中文>")
    });
    event(id, &run, &a2, &other, "SessionStart", 4);
    event(id, &run, &a1, &file, "Stop", 2);
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().unwrap().bound.as_ref() == Some(&a2)
            && w.current().unwrap().preview == "A2 独有"
    });
    press(&mut rt, "footprint-toggle", clock.elapsed().as_secs_f64());
    shown(&mut rt, clock, "copy-prompt-1");
    assert!(rt.presented("copy-prompt-2").is_none());
    drop(rt);

    // Neither an older thread nor a thread without its first prompt is in the recent ten.
    let empty = key("codex", 40);
    let empty_file = source(&roots, &empty);
    let mut store = crate::conversation_store::Store::open(&db, roots.clone()).unwrap();
    for n in 3..=38 {
        let k = key("agy", n);
        let path = source(&roots, &k);
        put(
            &path,
            prompt(&k, 0, "较新的对话", Some("2026-11-22T03:00:00Z")),
        );
        assert!(store.sync(&k, &dir, &path).unwrap());
    }
    put(
        &empty_file,
        json!({"type":"session_meta","payload":{"id":empty.id}}),
    );
    assert!(store.sync(&empty, &dir, &empty_file).unwrap());
    let recent = store.recent().unwrap();
    assert!(recent.iter().all(|r| r.key != a2 && r.key != empty));
    drop(store);

    // The previous run may have missed several transactions' worth of official writes.
    for n in 1..=520 {
        put(
            &other,
            prompt(&a2, n, "late row", Some("2026-11-22T04:00:00Z")),
        );
    }
    put(
        &empty_file,
        prompt(&empty, 0, "退出后的首条", Some("2026-11-22T05:00:00Z")),
    );
    let restarted = Service::start(db, roots, Arc::new(|| {}));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let update = restarted
            .receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("重启后应补读最近十段之外和原本为空的已登记对话");
        if let crate::history::Update::Recent(rows) = update
            && rows.iter().any(|row| row.key == a2 && row.count == 521)
            && rows
                .first()
                .is_some_and(|row| row.key == empty && row.count == 1)
        {
            break;
        }
    }
}

#[test]
fn passive_binding_covers_explicit_resume_and_fresh_codex_and_agy_sessions() {
    use crate::history::{Command, Update};
    let (dir, roots, db) = fixture("passive-bind");
    let explicit_key = key("codex", 10);
    let explicit_file = source(&roots, &explicit_key);
    put(
        &explicit_file,
        json!({"type":"session_meta","payload":{"id":explicit_key.id,"cwd":dir}}),
    );
    put(
        &explicit_file,
        prompt(&explicit_key, 0, "手动续接", Some("2026-11-24T01:00:00Z")),
    );
    {
        let mut store = crate::conversation_store::Store::open(&db, roots.clone()).unwrap();
        while store.sync(&explicit_key, &dir, &explicit_file).unwrap() {}
    }

    // Verify 3-field OSC report parses explicit ID without altering commands.
    let marker = SessionId::new().marker();
    let run = "00112233445566778899aabbccddeeff";
    let mut parser = shell::ShellReports::default();
    let mut launches = Vec::new();
    let osc = format!(
        "\x1b]633;P;McliAgent={marker}=codex={run}={}\x07",
        explicit_key.id
    );
    parser.feed(osc.as_bytes(), &marker, |r| {
        if let shell::Report::Launch(agent, r_run, id) = r {
            launches.push((agent, r_run, id));
        }
    });
    assert_eq!(
        launches,
        vec![(
            shell::Foreground::Codex,
            run.to_owned(),
            Some(explicit_key.id.clone())
        )]
    );

    let service = Service::start(db, roots.clone(), Arc::new(|| {}));
    let wait_bound = |want_marker: &str, want_key: &Conversation| {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let update = service
                .receive
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("应被动关联官方对话");
            if let Update::Bound(m, _, summary) = update
                && m == want_marker
                && &summary.key == want_key
            {
                return summary;
            }
        }
    };

    // 1. Manual explicit resume binds immediately.
    service
        .send
        .send(Command::Foreground {
            marker: marker.clone(),
            run: run.into(),
            agent: "codex".into(),
            project: dir.clone(),
            restore: None,
            initial: Some(explicit_key.clone()),
            proxy: None,
        })
        .unwrap();
    let s = wait_bound(&marker, &explicit_key);
    assert_eq!(s.count, 1);

    // 2. Fresh agy launch binds via read-only history.jsonl tail.
    let agy_marker = SessionId::new().marker();
    let agy_key = key("agy", 11);
    let agy_file = source(&roots, &agy_key);
    service
        .send
        .send(Command::Foreground {
            marker: agy_marker.clone(),
            run: run.into(),
            agent: "agy".into(),
            project: dir.clone(),
            restore: None,
            initial: None,
            proxy: None,
        })
        .unwrap();
    put(
        &agy_file,
        prompt(&agy_key, 0, "全新 agy 对话", Some("2026-11-24T02:00:00Z")),
    );
    let now_ms = (crate::hook_wire::timestamp_micros() / 1000) as i64;
    let history_jsonl = roots.agy.parent().unwrap().join("history.jsonl");
    put(
        &history_jsonl,
        json!({
            "display": "全新 agy 对话",
            "timestamp": now_ms,
            "workspace": dir,
            "conversationId": agy_key.id
        }),
    );
    let s = wait_bound(&agy_marker, &agy_key);
    assert_eq!(s.preview, "全新 agy 对话");
}

#[test]
fn osc7501_ignores_osc9_and_external_transcript_while_clearing_unread_on_rerun() {
    use crate::history::Command;
    use crate::shell::AgyEvent;
    let (dir, roots, db) = fixture("osc7501-authoritative");
    let key = key("agy", 21);
    let path = source(&roots, &key);
    let reply =
        |text| json!({"type":"PLANNER_RESPONSE","source":"MODEL","status":"DONE","content":text});
    put(
        &path,
        prompt(&key, 0, "初始提问", Some("2026-11-24T01:00:00Z")),
    );
    put(&path, reply("初始答复"));
    let front_id = SessionId::new();
    let agy_id = SessionId::new();
    let run = "00112233445566778899aabbccddeeff";
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    rt.update(|w, tx| {
        w.first_open = false;
        let service = Service::start(db, roots.clone(), w.wake.clone());
        let mut front = crate::workspace::Tab::pending(front_id, dir.clone(), mpsc::channel().1);
        front.opening = None;
        let mut tab = crate::workspace::Tab::pending(agy_id, dir.clone(), mpsc::channel().1);
        tab.opening = None;
        tab.run = Some(run.into());
        tab.foreground = Some(shell::Foreground::Agy);
        w.tabs.push(front);
        w.tabs.push(tab);
        service
            .send
            .send(Command::Foreground {
                marker: agy_id.marker(),
                run: run.into(),
                agent: "agy".into(),
                project: dir.clone(),
                restore: None,
                initial: Some(key.clone()),
                proxy: None,
            })
            .unwrap();
        // Startup OSC 7501 idle arrives before/around initial passive binding.
        service
            .send
            .send(Command::Agy(
                agy_id.marker(),
                run.into(),
                1_000_000,
                AgyEvent::ProgramStatus("就绪"),
            ))
            .unwrap();
        w.history.service = Some(service);
        w.select(Some(front_id), tx);
    });
    wait_for_workspace(&mut rt, clock, |w| {
        w.tabs[1].bound.as_ref() == Some(&key) && w.tabs[1].state_text() == "就绪"
    });
    assert!(!rt.state.tabs[1].unread);

    // 1. External Windows Terminal writes a new prompt to the same conversation:
    // prompt count updates, but this idle Mcli tab must NOT get locked into 运行中.
    put(
        &path,
        prompt(&key, 1, "外部终端提问", Some("2026-11-24T01:05:00Z")),
    );
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].prompt_count == 2);
    assert_eq!(rt.state.tabs[1].state_text(), "就绪");
    assert!(!rt.state.tabs[1].unread);

    // 2. This Mcli tab starts working with background subagents; an interim OSC 9 ready
    // notice must be ignored while OSC 7501 stays working.
    let send = rt.state.history.service.as_ref().unwrap().send.clone();
    send.send(Command::Agy(
        agy_id.marker(),
        run.into(),
        2_000_000,
        AgyEvent::ProgramStatus("运行中"),
    ))
    .unwrap();
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].state_text() == "运行中");
    send.send(Command::Agy(
        agy_id.marker(),
        run.into(),
        3_000_000,
        AgyEvent::Status("就绪"),
    ))
    .unwrap();
    for _ in 0..5 {
        rt.frame(clock.elapsed().as_secs_f64());
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(rt.state.tabs[1].state_text(), "运行中");
    assert!(!rt.state.tabs[1].unread);

    // 3. Background tasks finish -> OSC 7501 done -> background tab becomes unread.
    put(&path, reply("后台任务全部完成"));
    send.send(Command::Agy(
        agy_id.marker(),
        run.into(),
        4_000_000,
        AgyEvent::ProgramStatus("就绪"),
    ))
    .unwrap();
    wait_for_workspace(&mut rt, clock, |w| {
        w.tabs[1].state_text() == "就绪" && w.tabs[1].unread
    });

    // 4. Re-entering 运行中 immediately clears unread (never red dot + 运行中 together).
    send.send(Command::Agy(
        agy_id.marker(),
        run.into(),
        5_000_000,
        AgyEvent::ProgramStatus("运行中"),
    ))
    .unwrap();
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].state_text() == "运行中");
    assert!(!rt.state.tabs[1].unread);

    // 5. OSC 7501 error transitions to 出错 without marking an unread reply.
    send.send(Command::Agy(
        agy_id.marker(),
        run.into(),
        6_000_000,
        AgyEvent::ProgramStatus("出错"),
    ))
    .unwrap();
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].state_text() == "出错");
    assert!(!rt.state.tabs[1].unread);
}
