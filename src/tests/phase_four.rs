use super::*;
use crate::history::{Command, Service};
use crate::shell::AgyEvent;
use std::fs;

pub(crate) fn folder(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/phase-four-workbench/tests")
        .join(format!(
            "{label}-{}-{}",
            std::process::id(),
            SessionId::new().0
        ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn background_replies_clear_individually_from_recent_and_keyboard() {
    use super::conversation_index::{key, prompt, put, source};
    let root = folder("unread");
    let roots = crate::transcript::Roots {
        codex: root.join("codex"),
        agy: root.join("agy"),
        kimi: root.join("kimi"),
        grok: root.join("grok"),
    };
    let keys = [key("agy", 1), key("agy", 2), key("agy", 3)];
    let ids = [SessionId::new(), SessionId::new(), SessionId::new()];
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    rt.update(|w, tx| {
        w.first_open = false;
        let service = Service::start(root.join("index.sqlite3"), roots.clone(), w.wake.clone());
        for i in 0..3 {
            let project = root.join(if i == 0 { "front" } else { "background" });
            fs::create_dir_all(&project).unwrap();
            let file = source(&roots, &keys[i]);
            put(
                &file,
                prompt(&keys[i], 0, "测试项目摘要", Some("1970-01-01T00:00:01Z")),
            );
            let mut tab =
                crate::workspace::Tab::pending(ids[i], project.clone(), mpsc::channel().1);
            tab.opening = None;
            tab.run = Some(format!("run-{i}"));
            tab.foreground = Some(shell::Foreground::Agy);
            w.tabs.push(tab);
            let run = format!("run-{i}");
            service
                .send
                .send(Command::Foreground {
                    marker: ids[i].marker(),
                    run: run.clone(),
                    agent: "agy".into(),
                    project,
                    restore: None,
                    initial: None,
                    proxy: None,
                })
                .unwrap();
            service
                .send
                .send(Command::Agy(
                    ids[i].marker(),
                    run,
                    1_000_000,
                    AgyEvent::Thread(Some(keys[i].id.clone())),
                ))
                .unwrap();
        }
        w.history.service = Some(service);
        w.select(Some(ids[0]), tx);
    });
    wait_for_workspace(&mut rt, clock, |w| {
        w.tabs
            .iter()
            .all(|t| t.state_text() == "运行中" && t.bound.is_some())
    });
    let send = rt.state.history.service.as_ref().unwrap().send.clone();
    let status = |i: usize, at, value| {
        send.send(Command::Agy(
            ids[i].marker(),
            format!("run-{i}"),
            at,
            AgyEvent::Status(value),
        ))
        .unwrap()
    };
    let background = format!("project-{}", ids[1].0);
    press(&mut rt, &background, clock.elapsed().as_secs_f64());
    for i in 0..3 {
        status(i, 2_000_000, "就绪");
    }
    wait_for_workspace(&mut rt, clock, |w| {
        w.tabs.iter().all(|t| t.state_text() == "就绪")
    });
    assert_eq!(
        rt.state.tabs.iter().map(|t| t.unread).collect::<Vec<_>>(),
        [false, true, true]
    );
    assert!(rt.presented(&format!("session-{}", ids[1].0)).is_none());
    let task = format!("task-agy-{}", keys[1].id);
    assert!(
        rt.elements()
            .iter()
            .any(|e| e.key.as_deref() == Some(task.as_str())
                && e.label.as_deref().is_some_and(|l| l.contains("有新回复")))
    );
    press(&mut rt, &task, clock.elapsed().as_secs_f64());
    assert_eq!(rt.state.active, Some(ids[1]));
    assert_eq!(
        rt.state.tabs.iter().map(|t| t.unread).collect::<Vec<_>>(),
        [false, false, true]
    );
    rt.key(KeyEvent {
        key: Key::PageDown,
        mods: Mods {
            ctrl: true,
            ..Default::default()
        },
    });
    rt.frame(clock.elapsed().as_secs_f64());
    assert_eq!(rt.state.active, Some(ids[2]));
    assert!(rt.state.tabs.iter().all(|t| !t.unread));
    // A duplicate finish or a waiting/error transition cannot manufacture another unread reply.
    status(1, 3_000_000, "就绪");
    status(1, 4_000_000, "等待用户");
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].state_text() == "等待用户");
    status(1, 5_000_000, "就绪");
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].state_text() == "就绪");
    assert!(!rt.state.tabs[1].unread);
    status(1, 6_000_000, "运行中");
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].state_text() == "运行中");
    status(1, 7_000_000, "就绪");
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].unread);
    // Starting another turn while unselected clears the previous unread dot immediately.
    status(1, 8_000_000, "运行中");
    wait_for_workspace(&mut rt, clock, |w| w.tabs[1].state_text() == "运行中");
    assert!(!rt.state.tabs[1].unread);
}

#[test]
fn restore_three_projects_five_shells_twice_and_remove_closed_tab() {
    let root = folder("restore");
    let slots = root.join("windows");
    let projects: Vec<_> = ["项目 A", "B", "C"].map(|p| root.join(p)).into();
    for p in &projects {
        fs::create_dir_all(p).unwrap();
    }
    let order = [0, 1, 2, 0, 2];
    let new_window = || app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    {
        let mut rt = new_window();
        rt.update(|w, tx| {
            w.enable_restore(&slots);
            for i in order {
                w.new_shell(Some(projects[i].clone()), tx);
            }
            w.select(Some(w.tabs[1].id), tx);
            w.layout.collapsed.insert(projects[0].clone());
            w.save_window(tx);
        });
    }
    for pass in 0..2 {
        let mut rt = new_window();
        rt.update(|w, _| w.enable_restore(&slots));
        let clock = Instant::now();
        let count = if pass == 0 { 5 } else { 4 };
        wait_for_workspace(&mut rt, clock, |w| {
            w.tabs.len() == count && w.tabs.iter().all(|t| t.session.is_some()) && w.focused
        });
        assert_eq!(rt.state.active_index(), Some(1));
        assert!(rt.state.layout.collapsed.contains(&projects[0]));
        for (t, i) in rt.state.tabs.iter().zip(order) {
            assert_eq!(t.directory, projects[i]);
        }
        rt.ime(Ime::Commit(format!("restore-draft-{pass}")));
        wait_for_workspace(&mut rt, clock, |w| {
            frame(w.session().unwrap()).contains(&format!("restore-draft-{pass}"))
        });
        rt.update(|w, tx| {
            if pass == 0 {
                w.close(w.tabs[4].id, tx);
            }
            w.save_window(tx);
        });
        assert!(
            !fs::read_to_string(slots.join("1.json"))
                .unwrap()
                .contains("restore-draft")
        );
    }
}

#[test]
fn locked_windows_keep_distinct_ids_and_recover_from_a_corrupt_snapshot() {
    use crate::workspace_file::{Snapshot, WindowFile};
    let dir = folder("slots");
    let project = dir.join("one");
    let key_a = super::conversation_index::key("codex", 17);
    let key_b = super::conversation_index::key("codex", 18);
    let mut w = Workspace::open();
    for k in [&key_a, &key_b] {
        let mut t =
            crate::workspace::Tab::pending(SessionId::new(), project.clone(), mpsc::channel().1);
        t.bound = Some(k.clone());
        w.tabs.push(t);
    }
    let (first, snap, _) = WindowFile::claim(&dir).unwrap();
    assert!(snap.is_none());
    let (second, _, _) = WindowFile::claim(&dir).unwrap();
    first.save(&Snapshot::from_window(&w).unwrap()).unwrap();
    drop(first);
    let (again, snap, _) = WindowFile::claim(&dir).unwrap();
    let tabs = snap.unwrap().tabs;
    assert_eq!(tabs[0].conversation.as_ref(), Some(&key_a));
    assert_eq!(tabs[1].conversation.as_ref(), Some(&key_b));
    w.tabs.clear();
    second.save(&Snapshot::from_window(&w).unwrap()).unwrap();
    assert!(dir.join("2.json").is_file());
    drop(again);
    fs::write(dir.join("1.json"), b"{broken").unwrap();
    let (replacement, snap, warning) = WindowFile::claim(&dir).unwrap();
    assert!(snap.is_none() && warning.is_some());
    replacement
        .save(&Snapshot::from_window(&w).unwrap())
        .unwrap();
    assert_eq!(fs::read(dir.join("1.json")).unwrap(), b"{broken");
    assert!(dir.join("3.json").is_file());
}

#[test]
fn native_codex_identity_survives_title_animation_and_a_background_finish() {
    check_codex_background_finish(false);
}

#[test]
fn restored_codex_counts_pending_and_fast_official_replies() {
    check_codex_background_finish(true);
}

fn check_codex_background_finish(restored: bool) {
    use super::conversation_index::{prompt, put};
    let root = folder("codex-title");
    let roots = crate::transcript::Roots {
        codex: root.join("codex"),
        agy: root.join("agy"),
        kimi: root.join("kimi"),
        grok: root.join("grok"),
    };
    let key = crate::transcript::Key::new("codex", "01a201e9-c753-7872-ba0a-1a1bb35a0d7e").unwrap();
    let partition = roots.codex.join("sessions/2026/11/22");
    fs::create_dir_all(&partition).unwrap();
    let transcript = partition.join(format!("rollout-sample-{}.jsonl", key.id));
    let event = |name: &str| serde_json::json!({"type":"event_msg","payload":{"type":name,"last_agent_message":"一轮完成"}});
    put(
        &transcript,
        serde_json::json!({"type":"session_meta","payload":{"id":key.id,"cwd":root}}),
    );
    put(
        &transcript,
        prompt(&key, 0, "检查标题", Some("2026-11-23T02:00:00Z")),
    );
    put(&transcript, event("task_started"));
    let db = root.join("index.sqlite3");
    let mut store = crate::conversation_store::Store::open(&db, roots.clone()).unwrap();
    while store.sync(&key, &root, &transcript).unwrap() {}
    assert_eq!(
        store.known_codex_display(&key.id[..29]).unwrap().as_deref(),
        Some(key.id.as_str())
    );
    drop(store);
    let (a, b) = (SessionId::new(), SessionId::new());
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let start = Instant::now();
    rt.update(|w, tx| {
        w.first_open = false;
        for id in [a, b] {
            let mut t = crate::workspace::Tab::pending(id, root.clone(), mpsc::channel().1);
            t.opening = None;
            w.tabs.push(t);
        }
        let t = &mut w.tabs[1];
        t.expected = restored.then(|| key.clone());
        t.foreground = Some(shell::Foreground::Codex);
        t.run = Some("title-test".into());
        t.title = format!("{}... | 修复显示 | 修复显示", &key.id[..29]);
        let service = Service::start(db, roots, w.wake.clone());
        service
            .send
            .send(Command::Foreground {
                marker: b.marker(),
                run: "title-test".into(),
                agent: "codex".into(),
                project: root.clone(),
                restore: t.expected.clone(),
                initial: None,
                proxy: None,
            })
            .unwrap();
        service
            .send
            .send(Command::CodexTitle(
                b.marker(),
                "title-test".into(),
                shell::codex_display(&t.title).unwrap(),
            ))
            .unwrap();
        w.history.service = Some(service);
        w.select(Some(b), tx);
    });
    wait_for_workspace(&mut rt, start, |w| {
        w.tabs[1].bound.as_ref() == Some(&key) && w.tabs[1].state_text() == "运行中"
    });
    let frame = rt.state.spinner;
    wait_for_workspace(&mut rt, start, |w| w.spinner != frame);
    assert_eq!(rt.state.tabs[1].bound.as_ref(), Some(&key));
    assert!(rt.state.tabs[1].title.contains(&key.id[..29]));
    assert!(
        rt.state.tabs[1]
            .caption(rt.state.spinner)
            .starts_with(&format!(
                "{} 修复显示 · ",
                crate::sidebar::SPINNER[rt.state.spinner]
            ))
    );
    rt.update(|w, tx| w.select(Some(a), tx));
    put(&transcript, event("task_complete"));
    wait_for_workspace(&mut rt, start, |w| w.tabs[1].unread);
    assert_eq!(rt.state.tabs[1].state_text(), "就绪");
    let finished = rt.state.tabs[1].caption(rt.state.spinner);
    assert!(finished.starts_with("修复显示") && !finished.contains(&key.id[..29]));
    rt.update(|w, tx| w.select(Some(b), tx));
    rt.frame(start.elapsed().as_secs_f64() + 0.5);
    assert_eq!(rt.state.spinner, 0);
    assert!(!rt.state.tabs[1].unread);
    if restored {
        for (n, selected) in [(1, true), (2, false)] {
            rt.update(|w, tx| w.select(Some(if selected { b } else { a }), tx));
            put(&transcript, prompt(&key, n, "再问一次", None));
            put(&transcript, event("task_complete"));
            wait_for_workspace(&mut rt, start, |w| {
                w.tabs[1].prompt_count == n + 1 && w.tabs[1].unread != selected
            });
        }
        rt.update(|w, tx| w.select(Some(b), tx));
        assert!(!rt.state.tabs[1].unread);
    }
}

#[test]
#[ignore = "在专用目录重开已有的官方测试对话，输入 JSON 仅包含本轮允许恢复的样本"]
fn official_workbench_reopens_saved_threads() {
    let source = std::env::var_os("MCLI_PHASE4_RESTORE_CASES").expect("需要本轮测试样本");
    let cases: Vec<crate::workspace_file::SavedTab> =
        serde_json::from_reader(fs::File::open(source).unwrap()).unwrap();
    assert!(cases.len() >= 2);
    let slots = folder("official-restore");
    let new_window = || app().headless_runtime((1120., 720.), rune::Env::new(true, 1.));
    {
        let mut rt = new_window();
        rt.update(|w, tx| {
            w.enable_restore(&slots);
            w.first_open = false;
            for case in &cases {
                let k = case.conversation.as_ref().unwrap();
                assert_eq!(
                    crate::transcript::Key::new(&k.agent, &k.id).as_ref(),
                    Some(k)
                );
                let mut t = crate::workspace::Tab::pending(
                    SessionId::new(),
                    case.directory.clone(),
                    mpsc::channel().1,
                );
                t.bound = Some(k.clone());
                w.tabs.push(t);
            }
            w.select(Some(w.tabs[1].id), tx);
            w.save_window(tx);
        });
    }
    for pass in 1..=2 {
        let mut rt = new_window();
        rt.update(|w, _| {
            w.enable_restore(&slots);
            w.history.service = Some(Service::start(
                slots.join("test-index.sqlite3"),
                crate::transcript::Roots::installed().unwrap(),
                w.wake.clone(),
            ));
        });
        let start = Instant::now();
        loop {
            rt.frame(start.elapsed().as_secs_f64());
            if rt.state.tabs.len() == cases.len()
                && rt.state.tabs.iter().zip(&cases).all(|(t, c)| {
                    t.bound == c.conversation && t.prompt_count > 0 && t.session.is_some()
                })
            {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(75),
                "第 {pass} 轮恢复未完成，工作台提示 {:?}，各终端状态 {:?}",
                rt.state.history.error,
                rt.state
                    .tabs
                    .iter()
                    .map(|t| (
                        &t.foreground,
                        t.state_text(),
                        t.bound.is_some(),
                        t.prompt_count,
                        t.error.as_deref(),
                        t.session.as_ref().map(|s| {
                            let visible = frame(s).to_lowercase();
                            [
                                "已重试",
                                "恢复中断",
                                "找不到",
                                "无法读取",
                                "trust",
                                "auth",
                                "locked",
                                "permission denied",
                                "database",
                                "network",
                                "timed out",
                                "connection",
                                "unable",
                                "thread not found",
                                "error",
                                "panicked",
                                "press enter",
                            ]
                            .into_iter()
                            .filter(|word| visible.contains(word))
                            .collect::<Vec<_>>()
                        })
                    ))
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(45));
        }
        assert_eq!(rt.state.active_index(), Some(1));
        // Codex can replay its working/idle state after the official ID has appeared.
        let settle = Instant::now();
        while settle.elapsed() < Duration::from_secs(6) {
            rt.frame(start.elapsed().as_secs_f64());
            assert!(
                rt.state.tabs.iter().all(|t| !t.unread),
                "restoring old turns marked a tab unread on pass {pass}"
            );
            std::thread::sleep(Duration::from_millis(45));
        }
        for (t, c) in rt.state.tabs.iter().zip(&cases) {
            assert_eq!(
                crate::workspace::project_key(&t.directory),
                crate::workspace::project_key(&c.directory)
            );
            assert!(!t.unread);
        }
        println!(
            "pass {pass}, restored {} exact official IDs with recorded prompts",
            cases.len()
        );
        rt.update(Workspace::save_window);
    }
}

#[test]
fn shell_compatibility_with_custom_path_one_cli_and_an_existing_project_hook() {
    use std::process::Command as Process;
    if let Ok(expected) = std::env::var("MCLI_PHASE4_TEST_SHELL") {
        let root = PathBuf::from(std::env::var_os("MCLI_PHASE4_TEST_ROOT").unwrap());
        let dir = root.join("含 空格的项目");
        let profile = if expected == "5" {
            // Windows may repopulate ProgramFiles when it creates the child. Supply the
            // empty installation directory to the same resolver and launch real PS 5.1.
            shell::find_program(
                &std::env::var_os("PATH").unwrap(),
                Some(&root.join("empty-program-files")),
                std::env::var_os("SystemRoot").as_deref().map(Path::new),
            )
        } else {
            shell::program()
        }
        .unwrap();
        assert_eq!(
            profile
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            if expected == "5" {
                "powershell.exe"
            } else {
                "pwsh.exe"
            }
        );
        assert_eq!(
            crate::transcript::Roots::installed().unwrap().codex,
            root.join("private-codex")
        );
        let original = fs::read(dir.join(".agents/hooks.json")).unwrap();
        let mut args = shell::arguments_for(Some(&super::conversation_index::key("agy", 87)));
        args.insert(0, "-NoProfile".into());
        args.last_mut().unwrap().push_str(
            r#"
Set-Location -LiteralPath (Join-Path $env:MCLI_PHASE4_TEST_ROOT '含 空格的项目')
[void](prompt)
[Console]::WriteLine(('MCLI-PS-' + $PSVersionTable.PSVersion.Major))
codex
[Console]::WriteLine('MCLI-ENV-READY')
$null=[Console]::ReadLine()
"#,
        );
        let session =
            Session::spawn(&profile, &args, &root, Size::default(), Arc::new(|| {})).unwrap();
        wait_for(&session, 18, || frame(&session).contains("MCLI-ENV-READY"));
        let output = frame(&session);
        assert!(
            output.contains(&format!("MCLI-PS-{expected}"))
                && output.contains("MCLI-CUSTOM-CLI")
                && output.contains("找不到 agy"),
            "{output}"
        );
        assert_eq!(
            session.listener.shell.lock().unwrap().directory.as_ref(),
            Some(&dir)
        );
        assert_eq!(fs::read(dir.join(".agents/hooks.json")).unwrap(), original);
        return;
    }
    let root = folder("environment");
    let custom = root.join("custom tools");
    fs::create_dir_all(&custom).unwrap();
    fs::write(
        custom.join("codex.cmd"),
        b"@echo off\r\necho MCLI-CUSTOM-CLI\r\n",
    )
    .unwrap();
    let hooks = root.join("含 空格的项目/.agents");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(hooks.join("hooks.json"), b"{ \"hooks\": {} }\n").unwrap();
    let sys = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let seven = pwsh();
    for version in ["7", "5"] {
        let mut paths = vec![custom.clone(), sys.clone()];
        if version == "7" {
            paths.push(seven.parent().unwrap().into());
        }
        let result=Process::new(std::env::current_exe().unwrap())
            .args(["--exact","tests::phase_four::shell_compatibility_with_custom_path_one_cli_and_an_existing_project_hook","--nocapture"])
            .env("MCLI_PHASE4_TEST_SHELL",version)
            .env("MCLI_PHASE4_TEST_ROOT",&root)
            .env("PATH",std::env::join_paths(paths).unwrap())
            .env("CODEX_HOME",root.join("private-codex"))
            .output().unwrap();
        assert!(
            result.status.success(),
            "PS {version}: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
