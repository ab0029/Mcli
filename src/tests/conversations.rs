use super::*;
use crate::hook_pipe::Server;
use crate::hook_wire::{Notice, host};
use crate::transcript::Key;

#[test]
fn agy_ready_requires_a_complete_native_notification() {
    let mut parser = shell::ShellReports::default();
    let mut states = Vec::new();
    let mut directories = Vec::new();
    let bytes = concat!(
        "Antigravity CLI is ready for input\x07",
        "\x1b]9;unrelated notification\x07",
        "\x1b]9;Antigravity CLI is ready for input soon\x07",
        "\x1b]9;C:\\unrelated\x07",
        "\x1b]9;9;\"C:\\work\"\x1b\\",
        "\x1b]9;Antigravity CLI is ready for input\x07",
        "\x1b]9;Antigravity CLI is ready for input\x1b\\",
    );
    for byte in bytes.as_bytes().chunks(1) {
        parser.feed(byte, "unused", |event| match event {
            shell::Report::Status(Some(agent), state, shell::StatusSource::Osc9Ready) => {
                states.push((agent, state))
            }
            shell::Report::Directory(path) => directories.push(path),
            _ => panic!("unexpected report"),
        });
    }
    assert_eq!(states, [(shell::Foreground::Agy, "就绪"); 2]);
    assert_eq!(directories, [PathBuf::from(r"C:\work")]);
}

#[test]
fn embedded_powershell_scripts_have_no_bom_and_run_cleanly_under_command_flag() {
    let resume = Key::new("agy", "01234567-1234-4321-8765-1123456789ab").unwrap();
    for args in [
        shell::arguments(),
        shell::session_arguments(None, true),
        shell::session_arguments(Some(&resume), true),
    ] {
        let script = args.last().unwrap();
        assert!(
            !script.contains('\u{feff}'),
            "PowerShell -Command 脚本中不得包含 UTF-8 BOM"
        );
    }
    let pwsh = shell::program().unwrap();
    let mut args = shell::arguments();
    args.retain(|a| a != "-NoExit");
    args.insert(0, "-NoProfile".into());
    let output = std::process::Command::new(pwsh)
        .args(&args)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stderr.trim().is_empty(),
        "启动集成脚本产生错误输出: status={:?}, stderr={stderr:?}",
        output.status
    );
}

#[test]
fn native_status_queries_and_titles_require_the_expected_protocol() {
    let mut parser = shell::ShellReports::default();
    let mut query = Vec::new();
    let mut states = Vec::new();
    let mut children = Vec::new();
    let bytes = concat!(
        "\x1b]7501;?\x07",
        "\x1b]7501;?\x1b\\",
        "\x1b]7501;state=blocked:kind=permission:app=codex\x07",
        "\x1b]7501;state=working:app=other\x07",
        "\x1b]7501;state=working:id=subtask:app=antigravity\x07",
        "\x1b]7501;state=working:id=agent-0123456789abcdef01234567\x07",
        "\x1b]7501;state=done:id=agent-0123456789abcdef01234567\x07",
        "\x1b]7501;app=antigravity:state=working\x07",
        "\x1b]7501;app=agy:state=idle\x07",
        "\x1b]7501;app=kimi-code:state=working\x07",
        "\x1b]7501;app=kimi-code:state=blocked:kind=permission\x07",
        "\x1b]7501;app=grok-build:state=working\x07",
        "\x1b]7501;state=clear\x1b\\",
    )
    .as_bytes();
    for part in bytes.chunks(2) {
        parser.feed(part, "unused", |event| match event {
            shell::Report::StatusQuery(bell) => query.push(bell),
            shell::Report::Status(agent, txt, shell::StatusSource::Osc7501) => {
                states.push((agent, txt))
            }
            shell::Report::ChildStatus(agent, id, txt) => children.push((agent, id, txt)),
            _ => panic!("unexpected report"),
        });
    }
    assert_eq!(query, [true, false]);
    assert_eq!(
        states,
        [
            (Some(shell::Foreground::Codex), "等待用户"),
            (Some(shell::Foreground::Agy), "运行中"),
            (Some(shell::Foreground::Agy), "就绪"),
            (Some(shell::Foreground::Kimi), "运行中"),
            (Some(shell::Foreground::Kimi), "等待用户"),
            (Some(shell::Foreground::Grok), "运行中"),
            (None, ""),
        ]
    );
    assert_eq!(
        children,
        [
            (Some(shell::Foreground::Agy), "subtask".into(), "运行中"),
            (None, "agent-0123456789abcdef01234567".into(), "运行中"),
            (None, "agent-0123456789abcdef01234567".into(), "就绪"),
        ]
    );
    assert_eq!(
        shell::grok_title_state("⚠ Action Required - Refactor parser - grok"),
        Some("等待用户")
    );
    assert_eq!(
        shell::grok_title_state("⠋ Thinking - Refactor parser - grok"),
        Some("运行中")
    );
    assert_eq!(
        shell::grok_title_state("Running: cargo test - grok"),
        Some("运行中")
    );
    assert_eq!(
        shell::grok_title_state("Refactor parser - grok"),
        Some("就绪")
    );
    assert_eq!(shell::grok_title_state("grok"), Some("就绪"));
    assert_eq!(shell::grok_title_state("PowerShell 7"), None);
    assert_eq!(
        shell::grok_title_name("⠋ Thinking - Refactor parser - grok"),
        "Refactor parser"
    );
    assert_eq!(shell::grok_title_name("grok"), "");
    let id = "01a20209-0a7f-7234-abcd-0123456789ab";
    let prefix = &id[..29];
    assert_eq!(
        shell::codex_display(&format!("{prefix}... | {id}")),
        Some((prefix.into(), Some(id.into())))
    );
    assert_eq!(
        shell::codex_display(&format!("{prefix}... | name | name")),
        Some((prefix.into(), None))
    );
    assert_eq!(shell::codex_display(id), None);
    assert_eq!(
        shell::codex_display(&format!("{prefix}... | 多行对话名称 | 多行对")),
        Some((prefix.into(), None))
    );
    assert_eq!(
        shell::codex_display(&format!("{prefix}... | alpha | beta")),
        None
    );
}

#[test]
fn kimi_and_grok_transcripts_discovery_and_cwd_encoding() {
    use super::conversation_index::put;
    use crate::conversation_store::Store;
    use crate::transcript::{Roots, grok_decode_cwd_from_dirname, grok_encode_cwd_dirname};
    use serde_json::json;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/kimi-grok-tests")
        .join(format!(
            "transcripts-{}-{}",
            std::process::id(),
            SessionId::new().0
        ));
    let project = root.join("demo-project");
    std::fs::create_dir_all(&project).unwrap();
    let roots = Roots {
        codex: root.join("codex"),
        agy: root.join("agy"),
        kimi: root.join("kimi"),
        grok: root.join("grok"),
    };

    // 1. Kimi Code CLI wire.jsonl + state.json + archived/subagent filtering
    let kimi_id = "kimi_session_01";
    let kimi_key = Key::new("kimi", kimi_id).unwrap();
    assert_eq!(kimi_key.label(), "Kimi");
    assert_eq!(kimi_key.resume(), format!("kimi --session {kimi_id}"));
    let kimi_dir = roots.kimi.join("sessions/ws-hash").join(kimi_id);
    std::fs::create_dir_all(&kimi_dir).unwrap();
    std::fs::write(
        kimi_dir.join("state.json"),
        serde_json::to_vec(&json!({
            "cwd": project,
            "archived": false,
            "custom": {}
        }))
        .unwrap(),
    )
    .unwrap();
    let wire = kimi_dir.join("wire.jsonl");
    put(
        &wire,
        json!({
            "type": "turn.prompt",
            "origin": "user",
            "promptId": "prompt-1",
            "turnId": 1,
            "time": 1764000000000i64,
            "input": [{"type": "text", "text": "帮我检查构建脚本"}]
        }),
    );
    put(
        &wire,
        json!({
            "type": "context.append_message",
            "time": 1764000001000i64,
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "已完成检查"}]
            }
        }),
    );
    put(
        &wire,
        json!({
            "type": "turn.ended",
            "time": 1764000002000i64,
            "reason": "completed"
        }),
    );
    // Archived Kimi session should be skipped by discovery.
    let archived_dir = roots.kimi.join("sessions/ws-hash/kimi_archived");
    std::fs::create_dir_all(&archived_dir).unwrap();
    std::fs::write(
        archived_dir.join("state.json"),
        serde_json::to_vec(&json!({ "cwd": project, "archived": true })).unwrap(),
    )
    .unwrap();
    put(
        &archived_dir.join("wire.jsonl"),
        json!({ "type": "turn.prompt", "payload": { "text": "旧会话" } }),
    );
    // Subagent Kimi session should be skipped by discovery.
    let sub_dir = roots.kimi.join("sessions/ws-hash/kimi_subagent");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::write(
        sub_dir.join("state.json"),
        serde_json::to_vec(&json!({
            "cwd": project,
            "custom": { "parent_session_id": kimi_id }
        }))
        .unwrap(),
    )
    .unwrap();
    put(
        &sub_dir.join("wire.jsonl"),
        json!({ "type": "turn.prompt", "payload": { "text": "子任务" } }),
    );

    // 2. Grok Build CLI updates.jsonl + CWD encoding/decoding (short and >255B .cwd)
    let grok_id = "grok-sess-42";
    let grok_key = Key::new("grok", grok_id).unwrap();
    assert_eq!(grok_key.label(), "Grok");
    assert_eq!(grok_key.resume(), format!("grok --resume {grok_id}"));
    let proj_str = project.to_string_lossy();
    let encoded = grok_encode_cwd_dirname(&proj_str);
    let grok_cwd_dir = roots.grok.join("sessions").join(&encoded);
    let grok_sess_dir = grok_cwd_dir.join(grok_id);
    std::fs::create_dir_all(&grok_sess_dir).unwrap();
    assert_eq!(
        grok_decode_cwd_from_dirname(&grok_cwd_dir).as_deref(),
        Some(proj_str.as_ref())
    );
    // Verify >255-byte fallback with `.cwd` file.
    let long_cwd_dir = roots.grok.join("sessions/very-long-slug-0123456789abcdef");
    std::fs::create_dir_all(&long_cwd_dir).unwrap();
    std::fs::write(long_cwd_dir.join(".cwd"), format!("{proj_str}\n")).unwrap();
    assert_eq!(
        grok_decode_cwd_from_dirname(&long_cwd_dir).as_deref(),
        Some(proj_str.as_ref())
    );

    let updates = grok_sess_dir.join("updates.jsonl");
    put(
        &updates,
        json!({
            "timestamp": 1764000060000i64,
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": {
                        "type": "text",
                        "text": "ls -la",
                        "_meta": { "bash_command": true }
                    }
                }
            }
        }),
    );
    put(
        &updates,
        json!({
            "timestamp": 1764000065000i64,
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": { "type": "text", "text": "优化 Grok 状态识别" },
                    "_meta": { "prompt_index": 0 }
                }
            }
        }),
    );
    put(
        &updates,
        json!({
            "timestamp": 1764000066000i64,
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "已补充 OSC 0 状态映射" }
                }
            }
        }),
    );
    put(
        &updates,
        json!({
            "timestamp": 1764000067000i64,
            "method": "_x.ai/session/update",
            "params": {
                "update": {
                    "sessionUpdate": "turn_ended",
                    "outcome": "completed"
                }
            }
        }),
    );

    // Verify discovery enumerates exactly the two active main sessions.
    let found = crate::discovery::collect(&roots);
    assert_eq!(
        found.len(),
        2,
        "发现结果应仅包含 1 个 Kimi 和 1 个 Grok 主会话: {found:?}"
    );
    assert!(found.iter().any(|c| c.key == kimi_key));
    assert!(found.iter().any(|c| c.key == grok_key));

    // Verify Store indexing and prompt extraction for both agents.
    let db = root.join("index.sqlite3");
    let mut store = Store::open(&db, roots.clone()).unwrap();
    for item in &found {
        while store.sync(&item.key, &item.project, &item.path).unwrap() {}
    }
    let kimi_sum = store.get(&kimi_key).unwrap().unwrap();
    assert_eq!(kimi_sum.count, 1);
    assert_eq!(kimi_sum.phase, "就绪");
    assert_eq!(store.text(&kimi_key, 1).unwrap(), "帮我检查构建脚本");

    let grok_sum = store.get(&grok_key).unwrap().unwrap();
    assert_eq!(grok_sum.count, 1);
    assert_eq!(grok_sum.phase, "就绪");
    assert_eq!(store.text(&grok_key, 1).unwrap(), "优化 Grok 状态识别");
}

#[test]
fn grok_active_sessions_and_kimi_wire_bind_passively_without_cli_rewrite() {
    use super::conversation_index::put;
    use crate::history::{Command, Service, Update};
    use crate::transcript::{Roots, grok_encode_cwd_dirname};
    use serde_json::json;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/kimi-grok-tests")
        .join(format!(
            "passive-bind-{}-{}",
            std::process::id(),
            SessionId::new().0
        ));
    let project_grok = root.join("proj-grok");
    let project_kimi = root.join("proj-kimi");
    std::fs::create_dir_all(&project_grok).unwrap();
    std::fs::create_dir_all(&project_kimi).unwrap();
    let roots = Roots {
        codex: root.join("codex"),
        agy: root.join("agy"),
        kimi: root.join("kimi"),
        grok: root.join("grok"),
    };
    std::fs::create_dir_all(roots.grok.join("sessions")).unwrap();
    std::fs::create_dir_all(roots.kimi.join("sessions")).unwrap();

    let service = Service::start(
        root.join("index.sqlite3"),
        roots.clone(),
        std::sync::Arc::new(|| ()),
    );
    let grok_marker = SessionId::new().marker();
    let kimi_marker = SessionId::new().marker();

    service
        .send
        .send(Command::Foreground {
            marker: grok_marker.clone(),
            run: "run-grok".into(),
            agent: "grok".into(),
            project: project_grok.clone(),
            restore: None,
            initial: None,
            proxy: None,
        })
        .unwrap();
    service
        .send
        .send(Command::Foreground {
            marker: kimi_marker.clone(),
            run: "run-kimi".into(),
            agent: "kimi".into(),
            project: project_kimi.clone(),
            restore: None,
            initial: None,
            proxy: None,
        })
        .unwrap();

    // Grok registers active_sessions.json BEFORE updates.jsonl exists.
    let grok_id = "grok-live-001";
    std::fs::write(
        roots.grok.join("active_sessions.json"),
        serde_json::to_vec(&json!([{
            "session_id": grok_id,
            "pid": std::process::id(),
            "cwd": project_grok,
            "opened_at": "2026-11-24T12:05:00Z"
        }]))
        .unwrap(),
    )
    .unwrap();

    // Kimi creates state.json + wire.jsonl after launch.
    let kimi_id = "kimi-live-001";
    let kimi_dir = roots.kimi.join("sessions/ws-1").join(kimi_id);
    std::fs::create_dir_all(&kimi_dir).unwrap();
    std::fs::write(
        kimi_dir.join("state.json"),
        serde_json::to_vec(&json!({ "cwd": project_kimi })).unwrap(),
    )
    .unwrap();
    put(
        &kimi_dir.join("wire.jsonl"),
        json!({
            "type": "turn.prompt",
            "origin": "user",
            "promptId": "p-live-1",
            "time": 1764000301000i64,
            "input": [{"type": "text", "text": "Kimi 实时提示词"}]
        }),
    );

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut grok_bound = false;
    let mut grok_synced = false;
    let mut kimi_synced = false;
    let mut grok_updates_written = false;
    while Instant::now() < deadline && !(grok_synced && kimi_synced) {
        if let Ok(update) = service.receive.recv_timeout(Duration::from_millis(120)) {
            match update {
                Update::Bound(m, _, s)
                    if m == grok_marker && s.key.agent == "grok" && s.key.id == grok_id =>
                {
                    grok_bound = true;
                    if s.count == 1 {
                        grok_synced = true;
                    }
                    if !grok_updates_written {
                        grok_updates_written = true;
                        let enc = grok_encode_cwd_dirname(&project_grok.to_string_lossy());
                        let udir = roots.grok.join("sessions").join(enc).join(grok_id);
                        std::fs::create_dir_all(&udir).unwrap();
                        put(
                            &udir.join("updates.jsonl"),
                            json!({
                                "seq": 1,
                                "timestamp": "2026-11-24T12:05:02Z",
                                "update": {
                                    "sessionUpdate": "user_message_chunk",
                                    "content": { "type": "text", "text": "Grok 实时提示词" }
                                }
                            }),
                        );
                    }
                }
                Update::Bound(m, _, s)
                    if m == kimi_marker
                        && s.key.agent == "kimi"
                        && s.key.id == kimi_id
                        && s.count == 1 =>
                {
                    kimi_synced = true;
                }
                _ => (),
            }
        }
    }
    assert!(grok_bound, "Grok 应通过 active_sessions.json 被动绑定");
    assert!(
        grok_synced,
        "Grok 后续创建的 updates.jsonl 应自动被 Running::sync 发现并同步"
    );
    assert!(
        kimi_synced,
        "Kimi 应通过 state.json + wire.jsonl 被动绑定并同步提示词"
    );
}

#[test]
fn kimi_osc7501_subagent_and_grok_title_status_drive_tabs() {
    // 1. Verify Kimi OSC 7501 child subagent aggregation on a real ConPTY session:
    // root becomes idle, but active id=agent-... child keeps status "运行中" until done.
    let script = r#"
$e = [char]27
$b = [char]7
[Console]::Write("$e]633;P;McliAgent=$($env:MCLI_SESSION_ID)=kimi=0123456789abcdef0123456789abcdef$b")
Start-Sleep -Milliseconds 150
[Console]::Write("$e]7501;app=kimi-code:state=working$b")
[Console]::Write("$e]7501;state=working:id=agent-0123456789abcdef01234567$b")
[Console]::Write("$e]7501;app=kimi-code:state=idle$b")
Start-Sleep -Milliseconds 400
[Console]::Write("$e]7501;state=done:id=agent-0123456789abcdef01234567$b")
Start-Sleep -Milliseconds 300
"#;
    let (s, _) = fixture("kimi-subagent-osc7501", script, &[]);
    wait_for(&s, 10, || {
        s.listener.shell.lock().unwrap().status == Some((shell::Foreground::Kimi, "运行中"))
    });
    // Even after root goes idle, the active child keeps status "运行中" until child sends done.
    wait_for(&s, 10, || {
        s.listener.shell.lock().unwrap().status == Some((shell::Foreground::Kimi, "就绪"))
    });
    wait_for(&s, 10, || s.end.lock().unwrap().is_some());

    // 2. Verify Grok OSC 0 title status drives Tab state_text and background unread red dot.
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    rt.update(|w, _| {
        w.first_open = false;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut front =
            crate::workspace::Tab::pending(SessionId::new(), dir.clone(), mpsc::channel().1);
        front.opening = None;
        let mut bg = crate::workspace::Tab::pending(SessionId::new(), dir, mpsc::channel().1);
        bg.opening = None;
        bg.foreground = Some(shell::Foreground::Grok);
        bg.run = Some("run-grok-ui".into());
        assert_eq!(bg.label(), "Grok");
        assert_eq!(bg.state_text(), "在线");

        // Running via OSC 0 spinner title
        assert!(bg.update_title("⠋ Thinking - Fix bug - grok"));
        bg.observe_reply(false);
        assert_eq!(bg.state_text(), "运行中");
        assert!(!bg.unread);

        // Waiting for user action
        assert!(bg.update_title("⚠ Action Required - grok"));
        bg.observe_reply(false);
        assert_eq!(bg.state_text(), "等待用户");

        // Running again -> idle triggers background unread dot
        assert!(bg.update_title("Running: cargo check - grok"));
        bg.observe_reply(false);
        assert_eq!(bg.state_text(), "运行中");
        assert!(bg.update_title("Fix bug - grok"));
        bg.observe_reply(false);
        assert_eq!(bg.state_text(), "就绪");
        assert!(bg.unread, "后台 Grok 从运行中切回就绪时应点亮未读红点");

        // New running turn clears unread dot
        assert!(bg.update_title("⠙ Responding - grok"));
        bg.observe_reply(false);
        assert!(!bg.unread, "进入新一轮运行中时应清除未读红点");

        w.tabs.push(front);
        w.tabs.push(bg);
    });
}

#[test]
fn powershell_integration_detects_kimi_and_grok_without_rewriting() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/kimi-grok-tests")
        .join(format!(
            "ps-ast-{}-{}",
            std::process::id(),
            SessionId::new().0
        ));
    std::fs::create_dir_all(&root).unwrap();
    for bin in ["kimi.cmd", "grok.cmd"] {
        std::fs::write(root.join(bin), "@echo off\r\n").unwrap();
    }
    let pwsh = shell::program().unwrap();
    let probe = format!(
        r#"
$env:PATH = "{};" + $env:PATH
$env:MCLI_SESSION_ID = "win-test"
function global:PSConsoleHostReadLine {{ "" }}
{}
function Test-Line($line) {{
    $global:__McliAgentName = $null
    $global:__McliExplicitId = $null
    $global:__McliPreparedLine = $null
    __McliWatchForeground $line
    [PSCustomObject]@{{
        Line = $line
        Prepared = $global:__McliPreparedLine
        Agent = $global:__McliAgentName
        Explicit = $global:__McliExplicitId
    }}
}}
@(
    'kimi',
    'kimi -S sess_abc',
    'kimi --session=sess_eq',
    'kimi --version',
    'kimi doctor',
    'kimi -p "one shot"',
    'grok',
    'grok -r grok-123',
    'grok --resume=grok-456',
    'grok models',
    'grok agent',
    'grok -p "one shot"'
) | ForEach-Object {{ Test-Line $_ }} | ConvertTo-Json -Compress
"#,
        root.display(),
        include_str!("../shell_integration.ps1").trim_start_matches('\u{feff}')
    );
    let output = std::process::Command::new(pwsh)
        .args(["-NoLogo", "-NoProfile", "-Command", &probe])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "PowerShell 探测失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_start = stdout.find('[').expect("stdout should contain JSON array");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout[json_start..]).unwrap();
    let expect = [
        ("kimi", Some("kimi"), None),
        ("kimi -S sess_abc", Some("kimi"), Some("sess_abc")),
        ("kimi --session=sess_eq", Some("kimi"), Some("sess_eq")),
        ("kimi --version", None, None),
        ("kimi doctor", None, None),
        ("kimi -p \"one shot\"", None, None),
        ("grok", Some("grok"), None),
        ("grok -r grok-123", Some("grok"), Some("grok-123")),
        ("grok --resume=grok-456", Some("grok"), Some("grok-456")),
        ("grok models", None, None),
        ("grok agent", None, None),
        ("grok -p \"one shot\"", None, None),
    ];
    for (row, (line, agent, explicit)) in rows.iter().zip(expect.iter()) {
        assert_eq!(row["Line"].as_str(), Some(*line));
        assert_eq!(
            row["Prepared"].as_str(),
            Some(*line),
            "绝不得改写用户命令行"
        );
        assert_eq!(row["Agent"].as_str(), *agent, "line={line}");
        assert_eq!(row["Explicit"].as_str(), *explicit, "line={line}");
    }
}

#[test]
#[ignore = "需要已构建的 mcli-hook.exe，测量一次性程序的启动、IPC 和中立返回"]
fn shipped_hook_latency_with_host_online_and_offline() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exe = root.join("target/release/mcli-hook.exe");
    let marker = SessionId::new().marker();
    let (_, instance) = host(&marker).unwrap();
    let run = "92c95f9822534adca35054c153ae2811";
    let uid = "01234567-1234-4321-8765-1123456789ac";
    let mut result = Vec::new();
    for live in [false, true] {
        let (tx, rx) = mpsc::channel();
        let _server = live.then(|| {
            Server::start(&instance, move |n| {
                let _ = tx.send(n);
            })
            .unwrap()
        });
        for vendor in ["codex", "agy"] {
            let hook = if vendor == "codex" {
                serde_json::json!({"hook_event_name":"SessionStart","session_id":uid})
            } else {
                serde_json::json!({"conversationId":uid})
            };
            let mut us = Vec::new();
            for i in 0..23 {
                let now = Instant::now();
                let mut child = Command::new(&exe)
                    .args([vendor, "SessionStart"])
                    .env("MCLI_SESSION_ID", &marker)
                    .env("MCLI_LAUNCH_ID", run)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                let _ = child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(hook.to_string().as_bytes());
                let output = child.wait_with_output().unwrap();
                let elapsed = now.elapsed().as_micros();
                assert!(output.status.success());
                assert_eq!(
                    output.stdout,
                    if vendor == "agy" {
                        b"{}\n".as_slice()
                    } else {
                        &[]
                    }
                );
                if live {
                    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap().id, uid);
                }
                if i >= 3 {
                    us.push(elapsed);
                }
            }
            result.push(serde_json::json!({"provider":vendor,"online":live,"elapsed_us":us}));
        }
    }
    std::fs::write(
        root.join(".tmp/phase-three-conversations-and-prompts/hook-latency.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
}

fn native_shell(cwd: &Path) -> Session {
    let mut args = shell::arguments();
    args.last_mut().unwrap().push_str("\nSet-PSReadLineOption -HistorySaveStyle SaveNothing\n[Console]::WriteLine('MCLI-SHELL-READY')");
    let session = Session::spawn(
        &pwsh(),
        &args,
        cwd,
        Size {
            cols: 116,
            lines: 38,
            ..Size::default()
        },
        Arc::new(|| {}),
    )
    .unwrap();
    wait_for(&session, 20, || {
        frame(&session).contains("MCLI-SHELL-READY")
    });
    session
}
fn line(session: &Session, text: &str) {
    session.input(text.as_bytes());
    std::thread::sleep(Duration::from_millis(400));
    session.input(b"\r");
}
fn first_notice(session: &Session, receive: &mpsc::Receiver<Notice>, provider: &str) -> Notice {
    let deadline = Instant::now() + Duration::from_secs(55);
    loop {
        if let Ok(n) = receive.recv_timeout(Duration::from_millis(200)) {
            if n.marker == session.id.marker()
                && n.provider == provider
                && Key::new(provider, &n.id).is_some()
            {
                return n;
            }
        }
        if Instant::now() >= deadline {
            let view = frame(session);
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(".tmp/phase-three-conversations-and-prompts");
            let clean = |s: String| {
                s.lines()
                    .filter(|l| !l.contains('@'))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            std::fs::write(
                dir.join(format!("{provider}-probe-screen.txt")),
                clean(view.clone()),
            )
            .unwrap();
            if provider == "codex" {
                line(session, "/status");
                std::thread::sleep(Duration::from_secs(2));
                std::fs::write(dir.join("codex-probe-status.txt"), clean(frame(session))).unwrap();
            }
            panic!(
                "没有收到本次终端的正式通知。官方页面标志 {:?}",
                [
                    "for shortcuts",
                    "esc skip",
                    "trust",
                    "error",
                    "FAILED_PRECONDITION",
                    "MCLI-PHASE3"
                ]
                .into_iter()
                .filter(|s| view.contains(s))
                .collect::<Vec<_>>()
            );
        }
    }
}
fn collect_pair(provider: &str, command: &str, cwd: &Path) {
    let (send, rx) = mpsc::sync_channel(128);
    let a = native_shell(cwd);
    let (_, instance) = host(&a.id.marker()).unwrap();
    let _server = Server::start(&instance, move |n| {
        let _ = send.try_send(n);
    })
    .unwrap();
    let b = native_shell(cwd);
    let mut evidence = Vec::new();
    for (s, tag) in [(&a, "A"), (&b, "B")] {
        let mut launch = command.to_owned();
        if provider == "codex" && std::env::var_os("MCLI_TEST_HOOK_TRACE").is_some() {
            let log_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(".tmp/phase-three-conversations-and-prompts/codex-hook-log");
            std::fs::create_dir_all(&log_dir).unwrap();
            let override_value = format!(
                "log_dir=\"{}\"",
                log_dir.display().to_string().replace('\\', "/")
            );
            launch.push_str(&format!(" -c '{}'", override_value.replace('\'', "''")));
        }
        line(s, &launch);
        let ready = Instant::now() + Duration::from_secs(50);
        loop {
            let t = frame(s);
            if !t.contains("Do you trust")
                && (t.contains("for shortcuts")
                    || t.contains("Ask Codex")
                    || t.contains("esc skip")
                    || (t.contains("Antigravity CLI")
                        && t.lines().any(|l| l.trim_start().starts_with('>'))))
            {
                break;
            }
            assert!(Instant::now() < ready, "官方 CLI 尚未显示输入栏");
            std::thread::sleep(Duration::from_millis(100));
        }
        let screen = frame(s);
        if screen.contains("Update available") && screen.contains("esc skip") {
            s.input(b"\x1b");
        }
        std::thread::sleep(Duration::from_millis(950));
        line(
            s,
            &format!("MCLI-PHASE3-{tag}. Reply only with the word lilac. Use no tools."),
        );
        let n = first_notice(s, &rx, provider);
        evidence.push(format!(
            "{tag} {} {} {} {} {}",
            n.marker,
            n.run,
            n.event,
            n.id,
            n.path
                .as_deref()
                .map_or_else(String::new, |p| p.display().to_string())
        ));
    }
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp/phase-three-conversations-and-prompts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{}-pair.txt", command.replace(' ', "-"))),
        evidence.join("\n"),
    )
    .unwrap();
    assert_ne!(
        evidence[0].split_whitespace().nth(4),
        evidence[1].split_whitespace().nth(4)
    );
}
#[test]
#[ignore = "需要已注册 Mcli 钩子的专用 agy 项目，将发送两条合成提问"]
fn official_phase_three_agy_hooks() {
    collect_pair("agy", "agy", &official_test_project(Agent::Agy));
}
#[test]
#[ignore = "需要已注册并经官方审阅信任的 Codex 钩子，将发送两条合成提问"]
fn official_phase_three_codex_hooks() {
    collect_pair("codex", "codex", &official_test_project(Agent::Codex));
}

#[test]
#[ignore = "在专用项目中验证官方单次 --no-daemon 启动对两个终端身份的影响"]
fn official_phase_three_codex_embedded_hooks() {
    collect_pair(
        "codex",
        "codex --no-daemon",
        &official_test_project(Agent::Codex),
    );
}

#[test]
#[ignore = "验证两个官方 TUI 的完整线程标题与截短字段"]
fn official_phase_three_codex_title_ids() {
    let project = official_test_project(Agent::Codex);
    let a = native_shell(&project);
    let b = native_shell(&project);
    let mut ids = Vec::new();
    for (s, tag) in [(&a, "A"), (&b, "B")] {
        line(s, "codex");
        wait_for(s, 60, || {
            let view = frame(s);
            view.contains("for shortcuts")
                || view.contains("esc skip")
                || view.contains("Ask Codex")
        });
        if frame(s).contains("esc skip") {
            s.input(b"\x1b");
        }
        std::thread::sleep(Duration::from_millis(650));
        line(s, &format!("Mcli test {tag}. Reply OK and use no tools."));
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let title = s
                .listener
                .shell
                .lock()
                .unwrap()
                .title
                .clone()
                .unwrap_or_default();
            if let Some((_, Some(id))) = shell::codex_display(title.trim()) {
                ids.push(id);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "未得到独立终端的官方 UUID，标题 {title:?}"
            );
            std::thread::sleep(Duration::from_millis(90));
        }
    }
    assert_ne!(ids[0], ids[1]);
    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/phase-three-conversations-and-prompts/codex-title-ids.txt");
    std::fs::write(out, ids.join("\n")).unwrap();
}
