use super::*;

fn project(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("phase-two-multi-session")
        .join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn switch(rt: &mut rune::Runtime<Workspace>, id: SessionId, clock: Instant) {
    press(
        rt,
        &format!("session-{}", id.0),
        clock.elapsed().as_secs_f64(),
    );
    wait_for_workspace(rt, clock, |w| w.focused && w.active == Some(id));
}

#[test]
fn project_paths_preserve_distinct_directories_and_unc_shares() {
    let key = |s: &str| crate::workspace::project_key(Path::new(s));
    assert_eq!(key(r"c:/Work/中文/"), key(r"\\?\C:\Work\中文"));
    assert_ne!(key(r"C:\one\work"), key(r"C:\two\work"));
    assert_ne!(key(r"C:\Work\case"), key(r"C:\Work\CASE"));
    assert_eq!(key(r"\\HOST\Share\Dir"), key(r"\\?\UNC\host\share\Dir"));
    assert_ne!(key(r"\\host\Share1\Dir"), key(r"\\host\Share2\Dir"));
}

#[test]
fn deferred_paste_and_old_composition_never_cross_a_switch() {
    use rune::clipboard::{ClipboardContent, ClipboardImage};
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let board = rune::clipboard::memory();
    rt.use_clipboard(&board);
    let mut ids = Vec::new();
    let reader = [
        RAW_CONSOLE_PEER,
        r#"
$e=[string][char]27
[Console]::Write($e+'[?1004l'+$e+'[?2004l'+'READY')
$v=[Wire]::Read();[Console]::WriteLine();[Console]::WriteLine("RESULT<$v>")
"#,
    ]
    .concat();
    for name in ["multi-typed-a", "multi-typed-b"] {
        let (s, cwd) = fixture_in(project(name), &reader, &[]);
        wait_for(&s, 10, || frame(&s).contains("READY"));
        ids.push(s.id);
        rt.update(|w, tx| attach_session(w, s, &cwd, tx));
        wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    }
    let (a, b) = (ids[0], ids[1]);
    let path = ClipboardContent::default().with_files([PathBuf::from(r"L:\sample space\测试.txt")]);
    let header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01";
    let png =
        ClipboardContent::default().with_image(ClipboardImage::from_png(&header[..]).unwrap());
    for content in [
        ClipboardContent::new("late-text"),
        path.clone(),
        png.clone(),
    ] {
        board.set_content(content);
        rt.update(|w, tx| {
            w.paste(tx);
            w.select(Some(a), tx);
            w.select(Some(b), tx);
        });
        rt.frame(clock.elapsed().as_secs_f64());
        rt.frame(clock.elapsed().as_secs_f64());
    }
    rt.ime(Ime::Preedit("lin".into(), None, vec![]));
    rt.frame(clock.elapsed().as_secs_f64());
    switch(&mut rt, a, clock);
    rt.ime(Ime::Commit("迟到".into()));
    rt.ime(Ime::Preedit("jia".into(), None, vec![]));
    rt.ime(Ime::Commit("甲".into()));
    let verify = |rt: &mut rune::Runtime<Workspace>, key: &str, expected: &str| {
        rt.key(KeyEvent {
            key: Key::Text(key.into()),
            mods: Mods::default(),
        });
        let hex = expected
            .bytes()
            .map(|b| format!("{b:02X}"))
            .collect::<String>();
        wait_for_workspace(rt, clock, |w| {
            frame(w.session().unwrap())
                .replace('\n', "")
                .contains(&format!("RESULT<{hex}>"))
        });
    };
    verify(&mut rt, "-AWIREEND", "甲-AWIREEND");
    switch(&mut rt, b, clock);
    for content in [png, path] {
        board.set_content(content);
        rt.update(|w, tx| w.paste(tx));
        rt.frame(clock.elapsed().as_secs_f64());
        rt.frame(clock.elapsed().as_secs_f64());
    }
    verify(
        &mut rt,
        "BWIREEND",
        "\x16\"L:\\sample space\\测试.txt\"BWIREEND",
    );
}

#[test]
fn click_then_key_in_one_frame_reaches_only_the_selected_child() {
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let mut ids = Vec::new();
    for n in ["switch-key-a", "switch-key-b"] {
        let (s, cwd) = fixture_in(
            project(n),
            r#"[Console]::TreatControlCAsInput=$true;[Console]::WriteLine("KEYS-READY");$v=[Console]::ReadKey($true);[Console]::WriteLine(("GOT="+[int]$v.KeyChar));$null=[Console]::ReadLine()"#,
            &[],
        );
        wait_for(&s, 10, || frame(&s).contains("KEYS-READY"));
        ids.push(s.id);
        rt.update(|w, tx| attach_session(w, s, &cwd, tx));
        wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    }
    for (i, key, expected) in [
        (
            0,
            KeyEvent {
                key: Key::Command('c'),
                mods: Mods {
                    ctrl: true,
                    ..Default::default()
                },
            },
            3,
        ),
        (
            1,
            KeyEvent {
                key: Key::Text("R".into()),
                mods: Mods::default(),
            },
            82,
        ),
    ] {
        let keyname = format!("session-{}", ids[i].0);
        let ([x, y, width, height], _) = rt.presented(&keyname).unwrap();
        rt.pointer_down(x + width * 0.5, y + height * 0.5);
        rt.pointer_up(x + width * 0.5, y + height * 0.5);
        rt.key(key);
        rt.frame(clock.elapsed().as_secs_f64());
        wait_for_workspace(&mut rt, clock, |w| {
            w.active == Some(ids[i])
                && frame(w.session().unwrap())
                    .lines()
                    .any(|l| l == format!("GOT={expected}"))
        });
        if i == 0 {
            assert!(!frame(rt.state.tabs[1].session.as_ref().unwrap()).contains("GOT="));
        }
    }
}

#[test]
fn background_output_does_not_repaint_or_reset_scrollback_and_selection() {
    use std::rc::Rc;
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let (a, ad) = fixture_in(
        project("multi-output"),
        r#"[Console]::WriteLine("A-READY");$null=[Console]::ReadLine();1..160 | ForEach-Object { [Console]::WriteLine(([char]27 + "[31m" + "编号-{0:D3}" -f $_) + [char]27 + "[0m") };[Console]::WriteLine("A-DONE");$null=[Console]::ReadLine()"#,
        &[],
    );
    let aid = a.id;
    wait_for(&a, 10, || frame(&a).contains("A-READY"));
    rt.update(|w, tx| attach_session(w, a, &ad, tx));
    let (b, bd) = fixture_in(
        project("multi-scrolled"),
        r#"1..100 | ForEach-Object { [Console]::WriteLine("B-line-$_") }; [Console]::WriteLine("B-READY");$null=[Console]::ReadLine()"#,
        &[],
    );
    let bid = b.id;
    wait_for(&b, 10, || frame(&b).contains("B-READY"));
    rt.update(|w, tx| attach_session(w, b, &bd, tx));
    wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    let b = rt.state.session().unwrap();
    b.scroll(20);
    b.select(0, 2, false, Some(SelectionType::Simple));
    b.select(5, 2, true, None);
    let selected = b.selected().unwrap();
    let offset = b.term.lock().unwrap().grid().display_offset();
    rt.frame(clock.elapsed().as_secs_f64());
    let rows = rt.state.screen.rows.clone();
    let a = rt
        .state
        .tabs
        .iter()
        .find(|t| t.id == aid)
        .unwrap()
        .session
        .as_ref()
        .unwrap();
    a.input(b"\r");
    wait_for(a, 15, || frame(a).contains("A-DONE"));
    {
        let t = a.term.lock().unwrap();
        let data = (-i32::try_from(t.history_size()).unwrap()..t.screen_lines() as i32)
            .map(|n| line(&t, n))
            .filter(|l| l.starts_with("编号-"))
            .collect::<Vec<_>>();
        assert_eq!(
            data,
            (1..=160)
                .map(|n| format!("编号-{n:03}"))
                .collect::<Vec<_>>()
        );
    }
    rt.frame(clock.elapsed().as_secs_f64());
    assert!(
        rt.state
            .screen
            .rows
            .iter()
            .zip(&rows)
            .all(|(x, y)| Rc::ptr_eq(x, y))
    );
    rt.update(|w, tx| w.select(Some(aid), tx));
    assert!(
        rt.state
            .screen
            .rows
            .iter()
            .zip(&rows)
            .all(|(x, y)| !Rc::ptr_eq(x, y))
    );
    switch(&mut rt, bid, clock);
    let b = rt.state.session().unwrap();
    assert_eq!(b.term.lock().unwrap().grid().display_offset(), offset);
    assert_eq!(b.selected().as_deref(), Some(selected.as_str()));
}

#[test]
fn native_focus_reports_precede_input_after_switching() {
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let mut ids = vec![];
    let script = [
        RAW_CONSOLE_PEER,
        r#"
$e=[string][char]27
[Console]::Write($e+'[?1004h'+'FOCUS-READY');$received=[Wire]::Read()
[Console]::WriteLine();[Console]::WriteLine("BYTES=$received")
"#,
    ]
    .concat();
    for name in ["focus-wire-a", "focus-wire-b"] {
        let (s, cwd) = fixture_in(project(name), &script, &[]);
        wait_for(&s, 15, || frame(&s).contains("FOCUS-READY"));
        ids.push(s.id);
        rt.update(|w, tx| attach_session(w, s, &cwd, tx));
        wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    }
    for t in &rt.state.tabs {
        t.session.as_ref().unwrap().input(b"BEGIN");
    }
    for (i, c) in [(0, "j"), (1, "k")] {
        let r = rt.presented(&format!("session-{}", ids[i].0)).unwrap().0;
        rt.pointer_down(r[0] + r[2] * 0.5, r[1] + r[3] * 0.5);
        rt.pointer_up(r[0] + r[2] * 0.5, r[1] + r[3] * 0.5);
        rt.key(KeyEvent {
            key: Key::Text(c.into()),
            mods: Mods::default(),
        });
        rt.frame(clock.elapsed().as_secs_f64());
    }
    for (i, t) in rt.state.tabs.iter().enumerate() {
        let s = t.session.as_ref().unwrap();
        s.input(b"WIREEND");
        wait_for(s, 10, || s.end.lock().unwrap().is_some());
        let data = frame(s).replace('\n', "");
        let sequence = if i == 0 {
            "BEGIN\x1b[Ij\x1b[OWIREEND"
        } else {
            "BEGIN\x1b[O\x1b[IkWIREEND"
        };
        let hex = sequence
            .bytes()
            .map(|b| format!("{b:02X}"))
            .collect::<String>();
        assert!(data.contains(&hex), "焦点流 {i}: {data}");
    }
}

#[test]
fn hidden_fullscreen_resizes_to_the_current_window_and_restores_the_main_buffer() {
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let (a, ad) = fixture_in(
        project("switch-alt-screen"),
        r#"
$e=[string][char]27
[Console]::WriteLine("MAIN-OWNED"); $null=[Console]::ReadLine()
[Console]::Write($e+'[?1049h'+$e+'[H'+$e+'[32m'+'ALT-OWNED'+$e+'[0m'); $null=[Console]::ReadLine()
[Console]::WriteLine(('ALT-DIMS=' + [Console]::WindowWidth + 'x' + [Console]::WindowHeight)); $null=[Console]::ReadLine()
[Console]::Write($e+'[?1049l'); [Console]::WriteLine('RESTORED'); $null=[Console]::ReadLine()
"#,
        &[],
    );
    let aid = a.id;
    wait_for(&a, 10, || frame(&a).contains("MAIN-OWNED"));
    rt.update(|w, tx| attach_session(w, a, &ad, tx));
    wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    let (b, bd) = fixture_in(
        project("switch-alt-peer"),
        "[Console]::WriteLine('PEER');$null=[Console]::ReadLine()",
        &[],
    );
    rt.update(|w, tx| attach_session(w, b, &bd, tx));
    wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    let a = rt.state.tabs[0].session.as_ref().unwrap();
    a.input(b"\r");
    wait_for(a, 12, || {
        frame(a).contains("ALT-OWNED") && a.mode().contains(TermMode::ALT_SCREEN)
    });
    let previous = rt.state.size;
    rt.set_viewport((860., 505.), 1.);
    wait_for_workspace(&mut rt, clock, |w| {
        w.size.cols < previous.cols && w.size.lines < previous.lines
    });
    let size = rt.state.size;
    switch(&mut rt, aid, clock);
    let s = rt.state.session().unwrap();
    let columns = s.term.lock().unwrap().columns();
    assert_eq!(columns, usize::from(size.cols));
    s.input(b"\r");
    let dims = format!("ALT-DIMS={}x{}", size.cols, size.lines);
    wait_for(s, 10, || frame(s).contains(&dims));
    s.input(b"\r");
    wait_for(s, 10, || {
        frame(s).contains("RESTORED") && !s.mode().contains(TermMode::ALT_SCREEN)
    });
    assert!(frame(s).contains("MAIN-OWNED"));
}

#[test]
fn reverse_startup_cancel_and_background_close_preserve_selection() {
    let clock = Instant::now();
    let cwd = project("out-of-order");
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let (first, second) = (SessionId::new(), SessionId::new());
    let (send1, recv1) = mpsc::channel();
    let (send2, recv2) = mpsc::channel();
    rt.update(|w, tx| {
        w.add_pending(first, cwd.clone(), recv1, tx);
        w.add_pending(second, cwd.clone(), recv2, tx);
    });
    let args = [
        "-NoProfile",
        "-Command",
        "[Console]::WriteLine(('PID=' + $PID)); [void][Console]::ReadLine()",
    ]
    .map(str::to_owned);
    let spawn = |id| {
        Session::spawn_for(id, &pwsh(), &args, &cwd, Size::default(), Arc::new(|| {})).unwrap()
    };
    send2.send(Ok(spawn(second))).unwrap();
    wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    rt.update(|w, tx| w.close(first, tx));
    send1.send(Ok(spawn(first))).unwrap();
    wait_for_workspace(&mut rt, clock, |w| w.tabs.len() == 1);
    assert_eq!(rt.state.active, Some(second));
    assert_eq!(rt.state.session().unwrap().id, second);
    assert!(rt.state.focused);
}

#[test]
fn project_new_terminal_cd_and_close_work_through_the_sidebar() {
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let a = project("one/same");
    let b = project("two/same");
    let mut args = shell::arguments();
    args.last_mut()
        .unwrap()
        .push_str("\nSet-PSReadLineOption -HistorySaveStyle SaveNothing");
    let first = Session::spawn(&pwsh(), &args, &a, Size::default(), Arc::new(|| {})).unwrap();
    let aid = first.id;
    rt.update(|w, tx| attach_session(w, first, &a, tx));
    wait_for_workspace(&mut rt, clock, |w| {
        w.focused && w.current().unwrap().directory == a
    });
    press(
        &mut rt,
        &format!("project-new-{}", aid.0),
        clock.elapsed().as_secs_f64(),
    );
    wait_for_workspace(&mut rt, clock, |w| {
        w.tabs.len() == 2 && w.active != Some(aid) && w.focused && w.session().is_some()
    });
    let bid = rt.state.active.unwrap();
    assert_eq!(rt.state.groups().len(), 1);
    // First command disables persistence in this individual test shell.
    shell_line(
        &mut rt,
        "Set-PSReadLineOption -HistorySaveStyle SaveNothing",
    );
    shell_line(
        &mut rt,
        "Write-Output ('PROJECT-CHECK=' + $PWD.ProviderPath)",
    );
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).lines().any(|line| {
            line.strip_prefix("PROJECT-CHECK=")
                .is_some_and(|p| Path::new(p) == a)
        })
    });
    shell_line(
        &mut rt,
        &format!("Push-Location -LiteralPath {}", shell_quote(&b)),
    );
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().unwrap().directory == b && w.groups().len() == 2
    });
    switch(&mut rt, aid, clock);
    assert_eq!(rt.state.current().unwrap().directory, a);
    switch(&mut rt, bid, clock);
    shell_line(&mut rt, "Pop-Location");
    wait_for_workspace(&mut rt, clock, |w| {
        w.groups().len() == 1 && w.current().unwrap().directory == a
    });
    press(
        &mut rt,
        &format!("close-{}", aid.0),
        clock.elapsed().as_secs_f64(),
    );
    wait_for_workspace(&mut rt, clock, |w| w.tabs.len() == 1);
    assert_eq!(rt.state.active, Some(bid));
    shell_line(&mut rt, "exit 23");
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().unwrap().ended == Some(End::Code(23))
    });
    assert!(rt.presented(&format!("retry-{}", bid.0)).is_some());
}

pub(super) fn launch_profile_shell(rt: &mut rune::Runtime<Workspace>, cwd: &Path) -> SessionId {
    let mut args = shell::arguments();
    args.last_mut().unwrap().push_str("\nSet-PSReadLineOption -HistorySaveStyle SaveNothing\n[Console]::WriteLine(('SHELL-PID=' + $PID))");
    let s = Session::spawn(&pwsh(), &args, cwd, Size::default(), Arc::new(|| {})).unwrap();
    let id = s.id;
    rt.update(|w, tx| attach_session(w, s, cwd, tx));
    wait_for_workspace(rt, Instant::now(), |w| {
        w.focused
            && w.session()
                .is_some_and(|s| frame(s).lines().any(|line| line.starts_with("SHELL-PID=")))
    });
    id
}

#[test]
fn native_title_draft_and_detached_command_cannot_claim_foreground() {
    let cwd = project("agent-label/含 空格");
    let launcher = cwd.join("codex.cmd");
    std::fs::write(&launcher,"@echo off\r\nif \"%1\"==\"bg\" goto detached\r\necho SHIM-READY\r\nset /p reply=\r\nexit /b\r\n:detached\r\necho ready>\"%~dp0bg-%2.ready\"\r\nping -n 2 127.0.0.1 >nul\r\n").unwrap();
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    launch_profile_shell(&mut rt, &cwd);
    shell_line(&mut rt, "$Host.UI.RawUI.WindowTitle='Codex'");
    wait_for_workspace(&mut rt, clock, |w| w.current().unwrap().title == "Codex");
    assert_eq!(rt.state.current().unwrap().label(), "终端 · Codex");
    shell_line(
        &mut rt,
        &format!("Set-Alias qaagent {}", shell_quote(&launcher)),
    );
    rt.key(KeyEvent {
        key: Key::Text("qaagent".into()),
        mods: Mods::default(),
    });
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).contains("qaagent")
    });
    assert!(rt.state.current().unwrap().foreground.is_none());
    rt.key(KeyEvent {
        key: Key::Enter,
        mods: Mods::default(),
    });
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().unwrap().foreground == Some(shell::Foreground::Codex)
            && frame(w.session().unwrap()).contains("SHIM-READY")
    });
    shell_line(&mut rt, "ok");
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().unwrap().foreground.is_none()
    });
    let run = rt.state.active.unwrap().marker();
    shell_line(
        &mut rt,
        &format!(
            "Start-Process -WindowStyle Hidden -Wait -FilePath {} -ArgumentList 'bg','{run}'; Write-Output ('BACKGROUND-'+'DONE')",
            shell_quote(&launcher)
        ),
    );
    let marker = cwd.join(format!("bg-{run}.ready"));
    wait_for_workspace(&mut rt, clock, |_| marker.is_file());
    assert!(rt.state.current().unwrap().foreground.is_none());
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap())
            .lines()
            .any(|l| l == "BACKGROUND-DONE")
    });
    assert!(rt.state.current().unwrap().foreground.is_none());
}

#[test]
fn dropping_workspace_reaps_all_live_shells_and_pending_startups() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    let cwd = project("window-close");
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let mut handles = vec![];
    for i in 0..3 {
        let id = SessionId::new();
        let (send, recv) = mpsc::channel();
        rt.update(|w, tx| w.add_pending(id, cwd.clone(), recv, tx));
        let args = [
            "-NoProfile",
            "-Command",
            "[Console]::WriteLine(('OWNER=' + $PID));[void][Console]::ReadLine()",
        ]
        .map(str::to_owned);
        let s =
            Session::spawn_for(id, &pwsh(), &args, &cwd, Size::default(), Arc::new(|| {})).unwrap();
        wait_for(&s, 12, || {
            frame(&s).lines().any(|l| l.starts_with("OWNER="))
        });
        let pid = frame(&s)
            .lines()
            .find_map(|l| l.strip_prefix("OWNER=")?.parse::<u32>().ok())
            .unwrap();
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!h.is_null());
        handles.push(unsafe { OwnedHandle::from_raw_handle(h) });
        send.send(Ok(s)).unwrap();
        if i < 2 {
            wait_for_workspace(&mut rt, clock, |w| w.session().is_some());
        }
    }
    drop(rt);
    assert!(
        handles
            .iter()
            .all(|h| unsafe { WaitForSingleObject(h.as_raw_handle(), 5000) } == 0)
    );
}

fn ready_screen(agent: Agent, rt: &mut rune::Runtime<Workspace>, clock: Instant) {
    let deadline = Instant::now() + Duration::from_secs(50);
    let mut skipped = false;
    loop {
        rt.frame(clock.elapsed().as_secs_f64());
        let t = frame(rt.state.session().unwrap()).to_lowercase();
        assert!(!t.contains("do you trust") && !t.contains("trust this directory"));
        if !skipped && t.contains("update available") && t.contains("esc skip") {
            rt.key(KeyEvent {
                key: Key::Escape,
                mods: Mods::default(),
            });
            skipped = true;
        } else if matches!(agent, Agent::Agy) && t.contains("esc to dismiss") && !skipped {
            rt.key(KeyEvent {
                key: Key::Escape,
                mods: Mods::default(),
            });
            skipped = true;
        } else if !t.contains("esc skip")
            && match agent {
                Agent::Codex => {
                    t.contains("codex") && (t.contains("for shortcuts") || t.contains("ask codex"))
                }
                Agent::Agy => {
                    t.contains("antigravity")
                        && (t.contains("for shortcuts")
                            || t.contains("type a message")
                            || t.contains("mode: idle")
                            || t.contains('❯'))
                }
            }
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "{} 输入栏未就绪，侧栏为 {}，已看到官方标题={}，已关闭介绍或更新提示={skipped}",
                agent.name(),
                rt.state.current().unwrap().label(),
                t.contains("openai codex") || t.contains("antigravity cli")
            );
        }
        std::thread::sleep(Duration::from_millis(35));
    }
}

fn exit_official(rt: &mut rune::Runtime<Workspace>, clock: Instant) {
    for ch in "/exit".chars() {
        rt.key(KeyEvent {
            key: Key::Text(ch.to_string()),
            mods: Mods::default(),
        });
        rt.frame(clock.elapsed().as_secs_f64());
        std::thread::sleep(Duration::from_millis(55));
    }
    std::thread::sleep(Duration::from_millis(350));
    wait_for_workspace(rt, clock, |w| {
        frame(w.session().unwrap()).lines().any(|l| {
            l.trim_start()
                .strip_prefix(['›', '>', '❯'])
                .is_some_and(|text| text.trim() == "/exit")
        })
    });
    rt.key(KeyEvent {
        key: Key::Enter,
        mods: Mods::default(),
    });
    wait_for_workspace(rt, clock, |w| w.current().unwrap().foreground.is_none());
}

#[test]
#[ignore = "在两个已信任的目录中打开官方原生 CLI，检查草稿、切换和退出"]
fn official_two_sessions_preserve_native_drafts_and_shells() {
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1120., 680.), rune::Env::new(true, 1.));
    let mut terminals = vec![];
    for agent in [Agent::Codex, Agent::Agy] {
        let cwd = official_test_project(Agent::Agy);
        let id = launch_profile_shell(&mut rt, &cwd);
        let screen = frame(rt.state.session().unwrap());
        let pid = screen
            .lines()
            .find_map(|l| l.strip_prefix("SHELL-PID="))
            .unwrap()
            .to_owned();
        let cmd = agent.name().to_ascii_lowercase();
        shell_line(&mut rt, "$mcliStartDir=$PWD.ProviderPath");
        if agent == Agent::Codex {
            shell_line(
                &mut rt,
                &format!("codex -C {}", shell_quote(&official_test_project(agent))),
            );
        } else {
            shell_line(&mut rt, &cmd);
        }
        ready_screen(agent, &mut rt, clock);
        let title = rt.state.current().unwrap().title.to_ascii_lowercase();
        println!(
            "{cmd} title identifies codex={} agy={}",
            title.contains("codex"),
            title.contains("agy") || title.contains("antigravity")
        );
        assert_eq!(rt.state.current().unwrap().label(), agent.name());
        let draft = format!("DRAFT-{}-{}", "OWNED", id.0);
        rt.key(KeyEvent {
            key: Key::Text(draft.clone()),
            mods: Mods::default(),
        });
        wait_for_workspace(&mut rt, clock, |w| {
            frame(w.session().unwrap()).contains(&draft)
        });
        terminals.push((id, pid, cwd, draft));
    }
    assert_ne!(terminals[0].1, terminals[1].1);
    assert_eq!(rt.state.groups().len(), 1);
    for (i, (id, pid, cwd, draft)) in terminals.iter().enumerate() {
        switch(&mut rt, *id, clock);
        let content = frame(rt.state.session().unwrap());
        assert!(content.contains(draft));
        assert!(!content.contains(&terminals[1 - i].3));
        for _ in draft.bytes() {
            rt.key(KeyEvent {
                key: Key::Backspace,
                mods: Mods::default(),
            });
        }
        wait_for_workspace(&mut rt, clock, |w| {
            !frame(w.session().unwrap()).contains(draft)
        });
        exit_official(&mut rt, clock);
        shell_line(
            &mut rt,
            "Write-Output ('NATIVE-RETURN=' + $PID + ',' + ($PWD.ProviderPath -eq $mcliStartDir))",
        );
        let expected = format!("NATIVE-RETURN={pid},True");
        wait_for_workspace(&mut rt, clock, |w| {
            frame(w.session().unwrap()).contains(&expected)
        });
        assert_eq!(&rt.state.current().unwrap().directory, cwd);
        println!(
            "terminal {} returned to its original PowerShell {pid}",
            id.0
        );
    }
    shell_line(
        &mut rt,
        &format!(
            "codex -C {}",
            shell_quote(&official_test_project(Agent::Codex))
        ),
    );
    ready_screen(Agent::Codex, &mut rt, clock);
    assert_eq!(rt.state.current().unwrap().label(), "Codex");
    assert_eq!(rt.state.active, Some(terminals[1].0));
    exit_official(&mut rt, clock);
}
