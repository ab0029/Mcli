use super::*;
use crate::clawbot::ConnectionState;
use crate::clawbot::service::Service;
use crate::clawbot::store::AuthStore;
use crate::transcript::{Key, Roots, latest_reply};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

struct MockServerState {
    qr_statuses: VecDeque<Value>,
    updates: VecDeque<Value>,
    sent_messages: Vec<Value>,
}

struct MockIlinkServer {
    base_url: String,
    state: Arc<Mutex<MockServerState>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MockIlinkServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let state = Arc::new(Mutex::new(MockServerState {
            qr_statuses: VecDeque::new(),
            updates: VecDeque::new(),
            sent_messages: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let st = state.clone();
        let flag = stop.clone();
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let (path, body) = read_http_request(&mut stream);
                        let resp_json = handle_mock_route(&path, &body, &st);
                        let resp_bytes = serde_json::to_vec(&resp_json).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            resp_bytes.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&resp_bytes);
                        let _ = stream.flush();
                        let _ = stream.shutdown(std::net::Shutdown::Write);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{}", addr.port()),
            state,
            stop,
            thread: Some(handle),
        }
    }

    fn push_qr_status(&self, v: Value) {
        self.state.lock().unwrap().qr_statuses.push_back(v);
    }

    fn push_update(&self, v: Value) {
        self.state.lock().unwrap().updates.push_back(v);
    }

    fn sent_texts(&self) -> Vec<(String, String)> {
        self.state
            .lock()
            .unwrap()
            .sent_messages
            .iter()
            .map(|m| {
                let text = m["msg"]["item_list"][0]["text_item"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                let ctx = m["msg"]["context_token"].as_str().unwrap_or("").to_owned();
                (ctx, text)
            })
            .collect()
    }
}

impl Drop for MockIlinkServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let mut header_end = None;
    let mut content_len = 0usize;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if header_end.is_none()
                    && let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    header_end = Some(pos + 4);
                    let hdr = String::from_utf8_lossy(&raw[..pos]);
                    for line in hdr.lines() {
                        if let Some((k, v)) = line.split_once(':')
                            && k.trim().eq_ignore_ascii_case("content-length")
                        {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                if let Some(end) = header_end
                    && raw.len() >= end + content_len
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let Some(end) = header_end else {
        return (String::new(), Vec::new());
    };
    let head_str = String::from_utf8_lossy(&raw[..end]);
    let path = head_str
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("")
        .to_owned();
    let body = raw[end..].to_vec();
    (path, body)
}

fn handle_mock_route(path: &str, body: &[u8], state: &Arc<Mutex<MockServerState>>) -> Value {
    if path.starts_with("/ilink/bot/get_bot_qrcode") {
        return json!({
            "ret": 0,
            "qrcode": "mock_qr_001",
            "qrcode_img_content": "https://liteapp.weixin.qq.com/q/7GiQu1?qrcode=mock_qr_001&bot_type=3"
        });
    }
    if path.starts_with("/ilink/bot/get_qrcode_status") {
        let mut st = state.lock().unwrap();
        if let Some(next) = st.qr_statuses.pop_front() {
            return next;
        }
        return json!({ "ret": 0, "status": "wait" });
    }
    if path.starts_with("/ilink/bot/getupdates") {
        let mut st = state.lock().unwrap();
        if let Some(next) = st.updates.pop_front() {
            return next;
        }
        drop(st);
        thread::sleep(Duration::from_millis(60));
        return json!({ "ret": 0, "get_updates_buf": "buf_idle", "msgs": [] });
    }
    if path.starts_with("/ilink/bot/sendmessage") {
        if let Ok(v) = serde_json::from_slice::<Value>(body) {
            state.lock().unwrap().sent_messages.push(v);
        }
        return json!({ "ret": 0 });
    }
    if path.starts_with("/ilink/bot/getconfig") {
        return json!({ "ret": 0, "typing_ticket": "ticket_mock_1" });
    }
    if path.starts_with("/ilink/bot/sendtyping") {
        return json!({ "ret": 0 });
    }
    json!({ "ret": 0 })
}

macro_rules! wait_until {
    ($rt:expr, $timeout:expr, $pred:expr) => {{
        let start = Instant::now();
        let mut clock = 0.1;
        let mut ok = false;
        while start.elapsed() < $timeout {
            $rt.frame(clock);
            clock += 0.05;
            if ($pred)(&$rt.state) {
                ok = true;
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        ok
    }};
}

#[test]
fn mock_ilink_end_to_end_qr_login_commands_whitelist_and_session_expiry() {
    let server = MockIlinkServer::start();
    server.push_qr_status(json!({ "ret": 0, "status": "scaned" }));
    server.push_qr_status(json!({
        "ret": 0,
        "status": "confirmed",
        "bot_token": "tok_e2e_secret_12345",
        "ilink_bot_id": "bot_mcli@im.bot",
        "ilink_user_id": "owner_u1@im.wechat",
        "baseurl": server.base_url.clone()
    }));

    let data_dir = crate::tests::phase_four::folder("clawbot-e2e");
    let mut rt = app().headless_runtime((1040., 680.), rune::Env::new(true, 1.));
    rt.update(|w, tx| {
        w.first_open = false;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(
            data_dir.join("preferences.json"),
        ));
        w.layout.clawbot.base_url = server.base_url.clone();
        w.layout.clawbot_base_url_input = rune::TextEdit::new(server.base_url.clone());
        w.layout.settings = true;
        w.init_clawbot(data_dir.clone());
        // Add one synthetic Agent tab for deterministic command routing tests.
        let mut tab = crate::workspace::Tab::pending(
            SessionId::new(),
            data_dir.clone(),
            std::sync::mpsc::channel().1,
        );
        tab.opening = None;
        tab.foreground = Some(crate::shell::Foreground::Codex);
        tab.phase = "运行中".into();
        tab.preview = "正在检查构建".into();
        let id = tab.id;
        w.tabs.push(tab);
        w.select(Some(id), tx);
        w.clawbot_start_qr(tx);
    });

    assert!(
        wait_until!(rt, Duration::from_secs(6), |w: &Workspace| {
            matches!(w.clawbot.state, ConnectionState::Connected { .. })
        }),
        "expected Connected state, got {:?}",
        rt.state.clawbot.state
    );

    // Verify DPAPI file exists and does NOT contain plaintext bot_token.
    let store = AuthStore::in_dir(&data_dir);
    let raw = std::fs::read(store.path()).unwrap();
    assert!(
        !raw.windows(b"tok_e2e_secret_12345".len())
            .any(|w| w == b"tok_e2e_secret_12345")
    );

    // Verify second window cannot steal ownership lock.
    let mut prefs2 = rt.state.layout.clawbot.clone();
    prefs2.enabled = true;
    let svc2 = Service::start(data_dir.clone(), prefs2, Arc::new(|| {}));
    let ev2 = svc2.receive.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        ev2,
        crate::clawbot::service::Event::State(ConnectionState::TakenByOtherWindow)
    );
    drop(svc2);

    // Push: 1) unauthorized sender (must be rejected), 2) /list, 3) /use 1, 4) /sh (blocked), 5) plain prompt while running (rejected).
    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "buf_1",
        "msgs": [
            {
                "message_id": "m_bad",
                "message_type": 1,
                "from_user_id": "intruder@im.wechat",
                "context_token": "ctx_bad",
                "item_list": [{ "type": 1, "text_item": { "text": "/list" } }]
            },
            {
                "message_id": "m_1",
                "message_type": 1,
                "from_user_id": "owner_u1@im.wechat",
                "context_token": "ctx_list",
                "item_list": [{ "type": 1, "text_item": { "text": "/list" } }]
            },
            {
                "message_id": "m_2",
                "message_type": 1,
                "from_user_id": "owner_u1@im.wechat",
                "context_token": "ctx_use",
                "item_list": [{ "type": 1, "text_item": { "text": "/use 1" } }]
            },
            {
                "message_id": "m_3",
                "message_type": 1,
                "from_user_id": "owner_u1@im.wechat",
                "context_token": "ctx_sh",
                "item_list": [{ "type": 1, "text_item": { "text": "/sh whoami" } }]
            },
            {
                "message_id": "m_4",
                "message_type": 1,
                "from_user_id": "owner_u1@im.wechat",
                "context_token": "ctx_busy",
                "item_list": [{ "type": 1, "text_item": { "text": "继续分析" } }]
            }
        ]
    }));

    assert!(
        wait_until!(rt, Duration::from_secs(6), |_: &Workspace| {
            server.sent_texts().len() >= 4
        }),
        "sent messages: {:?}",
        server.sent_texts()
    );
    let sent = server.sent_texts();
    assert!(
        !sent.iter().any(|(c, _)| c == "ctx_bad"),
        "intruder must receive no reply"
    );
    assert!(
        sent.iter()
            .any(|(c, t)| c == "ctx_list" && t.contains("Codex"))
    );
    assert!(
        sent.iter()
            .any(|(c, t)| c == "ctx_use" && t.contains("已切换远程目标"))
    );
    assert!(
        sent.iter()
            .any(|(c, t)| c == "ctx_sh" && t.contains("安全策略已拦截"))
    );
    assert!(
        sent.iter()
            .any(|(c, t)| c == "ctx_busy" && t.contains("正在运行中"))
    );
    assert!(rt.state.clawbot.audit.iter().any(|e| e.kind == "拦截"));

    // Push -14 session timeout and verify graceful transition to SessionExpired.
    server.push_update(json!({ "errcode": -14, "errmsg": "session timeout" }));
    assert!(
        wait_until!(rt, Duration::from_secs(6), |w: &Workspace| {
            matches!(w.clawbot.state, ConnectionState::SessionExpired { .. })
        }),
        "expected SessionExpired, got {:?}",
        rt.state.clawbot.state
    );

    // Shutdown must complete promptly without hanging on 35s long-poll.
    let drop_start = Instant::now();
    drop(rt);
    assert!(
        drop_start.elapsed() < Duration::from_secs(3),
        "shutdown took too long: {:?}",
        drop_start.elapsed()
    );
}

#[test]
fn transcript_latest_reply_extracts_all_four_official_formats() {
    let root = crate::tests::phase_four::folder("clawbot-replies");
    let roots = Roots {
        codex: root.join("codex"),
        agy: root.join("agy"),
        kimi: root.join("kimi"),
        grok: root.join("grok"),
    };

    // 1. Codex
    let codex_id = "0195a000-0000-7000-8000-000000000001";
    let codex_key = Key::new("codex", codex_id).unwrap();
    let codex_file = roots
        .codex
        .join("sessions/2026/11/25")
        .join(format!("rollout-2026-11-25-{codex_id}.jsonl"));
    std::fs::create_dir_all(codex_file.parent().unwrap()).unwrap();
    std::fs::write(
        &codex_file,
        format!(
            "{}\n{}\n{}\n{}\n",
            json!({"type":"event_msg","timestamp":"2026-11-25T00:00:01Z","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"Q1"}]}}}),
            json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"OLD_CODEX"}}),
            json!({"type":"event_msg","timestamp":"2026-11-25T00:00:02Z","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u2","content":[{"type":"text","text":"Q2"}]}}}),
            json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"FINAL_CODEX_REPLY"}})
        ),
    )
    .unwrap();
    assert_eq!(
        latest_reply(&roots, &codex_key, &codex_file)
            .unwrap()
            .as_deref(),
        Some("FINAL_CODEX_REPLY")
    );

    // 2. agy
    let agy_id = "0195a000-0000-7000-8000-000000000002";
    let agy_key = Key::new("agy", agy_id).unwrap();
    let agy_file = roots
        .agy
        .join(agy_id)
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(agy_file.parent().unwrap()).unwrap();
    std::fs::write(
        &agy_file,
        format!(
            "{}\n{}\n{}\n{}\n",
            json!({"type":"USER_INPUT","source":"USER_EXPLICIT","step_index":1,"content":"Q1"}),
            json!({"type":"PLANNER_RESPONSE","source":"MODEL","status":"DONE","content":"OLD_AGY"}),
            json!({"type":"USER_INPUT","source":"USER_EXPLICIT","step_index":2,"content":"Q2"}),
            json!({"type":"PLANNER_RESPONSE","source":"MODEL","status":"DONE","content":"FINAL_AGY_REPLY"})
        ),
    )
    .unwrap();
    assert_eq!(
        latest_reply(&roots, &agy_key, &agy_file)
            .unwrap()
            .as_deref(),
        Some("FINAL_AGY_REPLY")
    );

    // 3. Kimi
    let kimi_key = Key::new("kimi", "kimi-sess-01").unwrap();
    let kimi_file = roots.kimi.join("sessions/ws1/kimi-sess-01/wire.jsonl");
    std::fs::create_dir_all(kimi_file.parent().unwrap()).unwrap();
    std::fs::write(
        &kimi_file,
        format!(
            "{}\n{}\n",
            json!({"type":"turn.prompt","origin":"user","promptId":"p1","input":[{"type":"text","text":"Q1"}]}),
            json!({"type":"context.append_message","message":{"role":"assistant","content":[{"type":"text","text":"FINAL_KIMI_REPLY"}]}})
        ),
    )
    .unwrap();
    assert_eq!(
        latest_reply(&roots, &kimi_key, &kimi_file)
            .unwrap()
            .as_deref(),
        Some("FINAL_KIMI_REPLY")
    );

    // 4. Grok (streaming chunks concatenated)
    let grok_key = Key::new("grok", "grok-sess-01").unwrap();
    let grok_file = roots.grok.join("sessions/ws1/grok-sess-01/updates.jsonl");
    std::fs::create_dir_all(grok_file.parent().unwrap()).unwrap();
    std::fs::write(
        &grok_file,
        format!(
            "{}\n{}\n{}\n",
            json!({"params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"Q1"}}}}),
            json!({"params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"HELLO "}}}}),
            json!({"params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"GROK!"}}}})
        ),
    )
    .unwrap();
    assert_eq!(
        latest_reply(&roots, &grok_key, &grok_file)
            .unwrap()
            .as_deref(),
        Some("HELLO GROK!")
    );
}

#[test]
fn clawbot_settings_persist_without_leaking_secrets_to_preferences_json() {
    let dir = crate::tests::phase_four::folder("clawbot-prefs");
    let prefs_path = dir.join("preferences.json");
    let mut rt = app().headless_runtime((1040., 680.), rune::Env::new(true, 1.));
    rt.update(|w, tx| {
        w.first_open = false;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path.clone()));
        w.layout.settings = true;
        w.set_clawbot_enabled(true, tx);
        w.set_clawbot_allow_raw_shell(true, tx);
        w.set_clawbot_notify_desktop_tasks(true, tx);
        w.set_clawbot_default_agent(Some("agy".into()), tx);
        w.layout.clawbot_default_dir_input = rune::TextEdit::new(r"L:\AI\Mcli");
        w.commit_clawbot_default_dir(tx);
        w.layout.clawbot_base_url_input = rune::TextEdit::new("http://127.0.0.1:9999");
        w.commit_clawbot_base_url(tx);
        w.layout.finish_width_write();
    });
    rt.frame(0.1);
    assert!(rt.presented("setting-clawbot-enabled").is_some());
    assert!(rt.presented("setting-clawbot-raw-shell").is_some());

    let raw = std::fs::read_to_string(&prefs_path).unwrap();
    assert!(!raw.contains("bot_token"));
    assert!(!raw.contains("ilink_user_id"));

    let reloaded = crate::workbench::Layout::new(rune::prefs::Prefs::at(prefs_path));
    assert!(reloaded.clawbot.enabled);
    assert!(reloaded.clawbot.allow_raw_shell);
    assert!(reloaded.clawbot.notify_desktop_tasks);
    assert_eq!(reloaded.clawbot.normalized_agent(), Some("agy"));
    assert_eq!(
        reloaded.clawbot.default_project.as_deref(),
        Some(Path::new(r"L:\AI\Mcli"))
    );
    assert_eq!(
        reloaded.clawbot.normalized_base_url(),
        "http://127.0.0.1:9999"
    );
}

#[test]
fn clawbot_real_conpty_screen_and_controlled_sh_roundtrip() {
    let server = MockIlinkServer::start();
    let data_dir = crate::tests::phase_four::folder("clawbot-conpty");
    // Pre-seed DPAPI auth so Service connects directly to mock server.
    let store = AuthStore::in_dir(&data_dir);
    store
        .save(&crate::clawbot::store::AuthRecord {
            bot_token: "tok_conpty".into(),
            ilink_bot_id: "bot_conpty".into(),
            ilink_user_id: "owner_conpty@im.wechat".into(),
            base_url: server.base_url.clone(),
            get_updates_buf: String::new(),
            last_context_token: None,
            last_inbound_ms: None,
            keepalive_sent_mask: 0,
        })
        .unwrap();

    let mut rt = app().headless_runtime((1040., 680.), rune::Env::new(true, 1.));
    rt.update(|w, tx| {
        w.first_open = false;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(
            data_dir.join("preferences.json"),
        ));
        w.layout.clawbot.enabled = true;
        w.layout.clawbot.allow_raw_shell = true;
        w.layout.clawbot.base_url = server.base_url.clone();
        w.new_shell(Some(data_dir.clone()), tx);
        w.init_clawbot(data_dir.clone());
    });

    assert!(wait_until!(rt, Duration::from_secs(12), |w: &Workspace| {
        w.tabs.first().is_some_and(|t| {
            t.session
                .as_ref()
                .is_some_and(|s| s.screen_snapshot(10).contains("PS "))
        }) && matches!(w.clawbot.state, ConnectionState::Connected { .. })
    }));
    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "buf_c1",
        "msgs": [
            {
                "message_id": "c1",
                "message_type": 1,
                "from_user_id": "owner_conpty@im.wechat",
                "context_token": "ctx_c1",
                "item_list": [{ "type": 1, "text_item": { "text": "/use 1" } }]
            },
            {
                "message_id": "c2",
                "message_type": 1,
                "from_user_id": "owner_conpty@im.wechat",
                "context_token": "ctx_c2",
                "item_list": [{ "type": 1, "text_item": { "text": "/sh Write-Output MCLI_WECHAT_OK_99" } }]
            }
        ]
    }));

    assert!(
        wait_until!(rt, Duration::from_secs(10), |_: &Workspace| {
            server
                .sent_texts()
                .iter()
                .any(|(c, t)| c == "ctx_c2" && t.contains("MCLI_WECHAT_OK_99"))
        }),
        "expected ConPTY output in WeChat reply, got: {:?}",
        server.sent_texts()
    );
}

#[test]
fn clawbot_proactive_keepalive_sends_before_expiry_and_ping_resets_window() {
    let server = MockIlinkServer::start();
    let data_dir = crate::tests::phase_four::folder("clawbot-keepalive");
    let store = AuthStore::in_dir(&data_dir);
    let twenty_one_half_hours_ms = (21 * 3600 + 1800) * 1000;
    let old_inbound =
        crate::clawbot::store::now_epoch_ms().saturating_sub(twenty_one_half_hours_ms);
    store
        .save(&crate::clawbot::store::AuthRecord {
            bot_token: "tok_ka".into(),
            ilink_bot_id: "bot_ka".into(),
            ilink_user_id: "owner_ka@im.wechat".into(),
            base_url: server.base_url.clone(),
            get_updates_buf: String::new(),
            last_context_token: Some("ctx_old_21h".into()),
            last_inbound_ms: Some(old_inbound),
            keepalive_sent_mask: 0,
        })
        .unwrap();

    let mut rt = app().headless_runtime((1040., 680.), rune::Env::new(true, 1.));
    rt.update(|w, _| {
        w.first_open = false;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(
            data_dir.join("preferences.json"),
        ));
        w.layout.clawbot.enabled = true;
        w.layout.clawbot.keepalive_remind = true;
        w.layout.clawbot.base_url = server.base_url.clone();
        w.init_clawbot(data_dir.clone());
    });

    assert!(
        wait_until!(rt, Duration::from_secs(6), |_: &Workspace| {
            server.sent_texts().iter().any(|(c, t)| {
                c == "ctx_old_21h" && t.contains("约 3 小时后过期") && t.contains("/ping")
            })
        }),
        "expected 3h keepalive reminder, got: {:?}",
        server.sent_texts()
    );

    // User replies /ping -> refreshes context_token, resets keepalive mask, and replies cleanly.
    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "buf_ka1",
        "msgs": [{
            "message_id": "ka_ping_1",
            "message_type": 1,
            "from_user_id": "owner_ka@im.wechat",
            "context_token": "ctx_fresh_24h",
            "item_list": [{ "type": 1, "text_item": { "text": "/ping" } }]
        }]
    }));
    assert!(
        wait_until!(rt, Duration::from_secs(6), |_: &Workspace| {
            server
                .sent_texts()
                .iter()
                .any(|(c, t)| c == "ctx_fresh_24h" && t.contains("续期 24 小时"))
        }),
        "expected /ping confirmation, got: {:?}",
        server.sent_texts()
    );
    let saved = store.load().unwrap().unwrap();
    assert_eq!(saved.last_context_token.as_deref(), Some("ctx_fresh_24h"));
    assert_eq!(saved.keepalive_sent_mask, 0);
}

#[test]
fn clawbot_restart_skips_stale_offline_msgs_auto_rebinds_and_handles_exited_tab() {
    let server = MockIlinkServer::start();
    let data_dir = crate::tests::phase_four::folder("clawbot-restart");
    let conv_id = "0195a000-0000-7000-8000-000000000077";
    let now_ms = crate::clawbot::store::now_epoch_ms();
    let store = AuthStore::in_dir(&data_dir);
    store
        .save(&crate::clawbot::store::AuthRecord {
            bot_token: "tok_restart".into(),
            ilink_bot_id: "bot_restart".into(),
            ilink_user_id: "owner_r@im.wechat".into(),
            base_url: server.base_url.clone(),
            get_updates_buf: "buf_prev".into(),
            last_context_token: Some("ctx_prev_active".into()),
            last_inbound_ms: Some(now_ms.saturating_sub(10 * 60 * 1000)),
            keepalive_sent_mask: 0,
        })
        .unwrap();

    // Push 1 stale offline message (5 minutes before start) + 1 fresh live message.
    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "buf_after_restart",
        "msgs": [
            {
                "message_id": "stale_1",
                "message_type": 1,
                "from_user_id": "owner_r@im.wechat",
                "context_token": "ctx_stale",
                "create_time_ms": now_ms.saturating_sub(5 * 60 * 1000),
                "item_list": [{ "type": 1, "text_item": { "text": "/stop" } }]
            },
            {
                "message_id": "live_1",
                "message_type": 1,
                "from_user_id": "owner_r@im.wechat",
                "context_token": "ctx_live",
                "create_time_ms": now_ms + 500,
                "item_list": [{ "type": 1, "text_item": { "text": "/status" } }]
            }
        ]
    }));

    let mut rt = app().headless_runtime((1040., 680.), rune::Env::new(true, 1.));
    let tab_id = SessionId::new();
    rt.update(|w, tx| {
        w.first_open = false;
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(
            data_dir.join("preferences.json"),
        ));
        w.layout.clawbot.enabled = true;
        w.layout.clawbot.base_url = server.base_url.clone();
        w.layout.clawbot.last_bound = Some(crate::clawbot::LastBoundTarget {
            directory: data_dir.clone(),
            agent: Some("codex".into()),
            conversation_id: Some(conv_id.into()),
        });
        let mut tab =
            crate::workspace::Tab::pending(tab_id, data_dir.clone(), std::sync::mpsc::channel().1);
        tab.opening = None;
        tab.foreground = Some(crate::shell::Foreground::Codex);
        tab.bound = Key::new("codex", conv_id);
        tab.phase = "就绪".into();
        w.tabs.push(tab);
        w.select(Some(tab_id), tx);
        w.init_clawbot(data_dir.clone());
    });

    // Wait for auto-rebind, offline-skip notice, live /status reply, and startup online notice.
    assert!(
        wait_until!(rt, Duration::from_secs(6), |w: &Workspace| {
            let sent = server.sent_texts();
            w.clawbot.bound_tab == Some(tab_id)
                && sent
                    .iter()
                    .any(|(_, t)| t.contains("已跳过离线期间积压的 1 条旧消息"))
                && sent
                    .iter()
                    .any(|(c, t)| c == "ctx_live" && t.contains("前台工具：Codex"))
                && sent.iter().any(|(_, t)| t.contains("已重新启动并恢复连接"))
        }),
        "bound={:?}, sent={:?}",
        rt.state.clawbot.bound_tab,
        server.sent_texts()
    );
    assert!(
        !server
            .sent_texts()
            .iter()
            .any(|(_, t)| t.contains("Ctrl+C")),
        "stale /stop must never execute"
    );

    // Simulate user typing `exit` inside the bound tab -> prompt must clearly report exited + /reopen.
    rt.update(|w, _| {
        w.tabs[0].ended = Some(crate::terminal::End::Code(0));
        w.tabs[0].foreground = None;
    });
    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "buf_exited",
        "msgs": [{
            "message_id": "live_after_exit",
            "message_type": 1,
            "from_user_id": "owner_r@im.wechat",
            "context_token": "ctx_exited",
            "create_time_ms": crate::clawbot::store::now_epoch_ms(),
            "item_list": [{ "type": 1, "text_item": { "text": "继续分析" } }]
        }]
    }));
    assert!(
        wait_until!(rt, Duration::from_secs(6), |_: &Workspace| {
            server
                .sent_texts()
                .iter()
                .any(|(c, t)| c == "ctx_exited" && t.contains("已退出") && t.contains("/reopen"))
        }),
        "expected exited tab notice, got: {:?}",
        server.sent_texts()
    );

    // /reopen restores a fresh tab in the same directory with the original official conversation key.
    let before_count = rt.state.tabs.len();
    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "buf_reopen",
        "msgs": [{
            "message_id": "live_reopen",
            "message_type": 1,
            "from_user_id": "owner_r@im.wechat",
            "context_token": "ctx_reopen",
            "create_time_ms": crate::clawbot::store::now_epoch_ms(),
            "item_list": [{ "type": 1, "text_item": { "text": "/reopen" } }]
        }]
    }));
    assert!(
        wait_until!(rt, Duration::from_secs(6), |w: &Workspace| {
            w.tabs.len() == before_count + 1
                && w.clawbot.bound_tab != Some(tab_id)
                && w.tabs
                    .last()
                    .and_then(|t| t.expected.as_ref())
                    .is_some_and(|k| k.agent == "codex" && k.id == conv_id)
                && server
                    .sent_texts()
                    .iter()
                    .any(|(c, t)| c == "ctx_reopen" && t.contains("按原对话 ID 续接 Codex"))
        }),
        "reopen failed: tabs={}, sent={:?}",
        rt.state.tabs.len(),
        server.sent_texts()
    );
}

#[test]
fn clawbot_proxy_command_switches_shell_and_reopens_agent_with_same_id() {
    let server = MockIlinkServer::start();
    let data_dir = crate::tests::phase_four::folder("clawbot-proxy");
    let conv_id = "0195a000-0000-7000-8000-000000000088";
    let store = AuthStore::in_dir(&data_dir);
    store
        .save(&crate::clawbot::store::AuthRecord {
            bot_token: "tok_prx".into(),
            ilink_bot_id: "bot_prx".into(),
            ilink_user_id: "owner_prx@im.wechat".into(),
            base_url: server.base_url.clone(),
            get_updates_buf: String::new(),
            last_context_token: None,
            last_inbound_ms: None,
            keepalive_sent_mask: 0,
        })
        .unwrap();

    let mut rt = app().headless_runtime((1040., 680.), rune::Env::new(true, 1.));
    let shell_id = SessionId::new();
    let agent_id = SessionId::new();
    rt.update(|w, tx| {
        w.first_open = false;
        w.enable_restore(&data_dir.join("workspaces"));
        w.layout = crate::workbench::Layout::new(rune::prefs::Prefs::at(
            data_dir.join("preferences.json"),
        ));
        w.layout.proxy.upsert_profile("p1", "10808", "").unwrap();
        w.layout
            .proxy
            .upsert_profile("p2", "http://127.0.0.1:7890", "")
            .unwrap();
        w.layout.clawbot.enabled = true;
        w.layout.clawbot.allow_raw_shell = false;
        w.layout.clawbot.base_url = server.base_url.clone();

        let mut sh = crate::workspace::Tab::pending(
            shell_id,
            data_dir.clone(),
            std::sync::mpsc::channel().1,
        );
        sh.opening = None;
        let mut ag = crate::workspace::Tab::pending(
            agent_id,
            data_dir.clone(),
            std::sync::mpsc::channel().1,
        );
        ag.opening = None;
        ag.foreground = Some(crate::shell::Foreground::Codex);
        ag.bound = Key::new("codex", conv_id);
        ag.phase = "运行中".into();
        w.tabs.push(sh);
        w.tabs.push(ag);
        w.clawbot.bound_tab = Some(shell_id);
        w.select(Some(shell_id), tx);
        w.init_clawbot(data_dir.clone());
    });

    // 1. `/proxy p1` on plain PowerShell succeeds even when allow_raw_shell == false.
    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "p1",
        "msgs": [{
            "message_id": "m_prx_1",
            "message_type": 1,
            "from_user_id": "owner_prx@im.wechat",
            "context_token": "ctx_p1",
            "item_list": [{ "type": 1, "text_item": { "text": "/proxy p1" } }]
        }]
    }));
    assert!(
        wait_until!(rt, Duration::from_secs(6), |w: &Workspace| {
            w.tabs[0].proxy_summary == "http://127.0.0.1:10808"
                && server
                    .sent_texts()
                    .iter()
                    .any(|(c, t)| c == "ctx_p1" && t.contains("p1 (http://127.0.0.1:10808)"))
        }),
        "expected shell proxy switch, got: {:?}",
        server.sent_texts()
    );

    // 2. Switch bound target to running Codex tab -> `/proxy p2` refused, `/proxy p2 restart` reopens with same ID.
    rt.update(|w, _| w.clawbot.bound_tab = Some(agent_id));
    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "p2",
        "msgs": [{
            "message_id": "m_prx_2",
            "message_type": 1,
            "from_user_id": "owner_prx@im.wechat",
            "context_token": "ctx_p2_refuse",
            "item_list": [{ "type": 1, "text_item": { "text": "/proxy p2" } }]
        }]
    }));
    assert!(
        wait_until!(rt, Duration::from_secs(6), |_: &Workspace| {
            server.sent_texts().iter().any(|(c, t)| {
                c == "ctx_p2_refuse" && t.contains("正在运行中") && t.contains("restart")
            })
        }),
        "expected running guard, got: {:?}",
        server.sent_texts()
    );

    server.push_update(json!({
        "ret": 0,
        "get_updates_buf": "p3",
        "msgs": [{
            "message_id": "m_prx_3",
            "message_type": 1,
            "from_user_id": "owner_prx@im.wechat",
            "context_token": "ctx_p2_go",
            "item_list": [{ "type": 1, "text_item": { "text": "/proxy p2 restart" } }]
        }]
    }));
    assert!(
        wait_until!(rt, Duration::from_secs(6), |w: &Workspace| {
            w.clawbot.bound_tab != Some(agent_id)
                && w.tabs.iter().any(|t| {
                    !t.closing
                        && t.proxy_summary == "http://127.0.0.1:7890"
                        && t.expected
                            .as_ref()
                            .is_some_and(|k| k.agent == "codex" && k.id == conv_id)
                })
                && server
                    .sent_texts()
                    .iter()
                    .any(|(c, t)| c == "ctx_p2_go" && t.contains("按原官方对话 ID 续接 Codex"))
        }),
        "expected agent reopen with same official ID, got: {:?}",
        server.sent_texts()
    );
}
