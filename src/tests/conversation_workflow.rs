use super::*;
use crate::conversation_store::Store;
use crate::history::Service;
use crate::transcript::{Key as Conversation, Roots};

struct Scene {
    rt: rune::Runtime<Workspace>,
    clock: Instant,
    project: PathBuf,
    root: PathBuf,
    db: PathBuf,
    outcomes: Vec<(&'static str, bool)>,
}
impl Scene {
    fn open(project: PathBuf, root: PathBuf) -> Self {
        let db = root.join("conversations.sqlite3");
        Self::open_with_database(project, root, db)
    }
    fn open_with_database(project: PathBuf, root: PathBuf, db: PathBuf) -> Self {
        Self::open_with_roots(project, root, db, Roots::installed().unwrap())
    }
    fn open_with_roots(project: PathBuf, root: PathBuf, db: PathBuf, roots: Roots) -> Self {
        std::fs::create_dir_all(&root).unwrap();
        let mut rt = app().headless_runtime((1120., 720.), rune::Env::new(true, 1.));
        rt.update(|w, _| {
            w.first_open = false;
            w.history.service = Some(Service::start(db.clone(), roots, w.wake.clone()));
        });
        Self {
            rt,
            clock: Instant::now(),
            project,
            root,
            db,
            outcomes: Vec::new(),
        }
    }
    fn spawn(&mut self) -> SessionId {
        let mut args = shell::arguments();
        args.last_mut().unwrap().push_str("\nSet-PSReadLineOption -HistorySaveStyle SaveNothing\n[Console]::WriteLine(('MCLI-SHELL-PID=' + $PID))");
        if std::env::var_os("MCLI_TEST_HOOK_TRACE").is_some() {
            let path = shell_quote(&self.root.join("launches.txt"));
            args.last_mut().unwrap().push_str(&format!("\n$global:McliTestEditor=$function:PSConsoleHostReadLine\nfunction global:PSConsoleHostReadLine {{ $line=& $global:McliTestEditor; if ($line -match '^(codex|agy)( |$)') {{ [IO.File]::AppendAllText({path},$line+[Environment]::NewLine) }}; $line }}"));
        }
        let s = Session::spawn(
            &pwsh(),
            &args,
            &self.project,
            Size {
                cols: 116,
                lines: 38,
                ..Size::default()
            },
            Arc::new(|| {}),
        )
        .unwrap();
        let id = s.id;
        self.rt
            .update(|w, tx| attach_session(w, s, &self.project, tx));
        self.until(id, "shell", 30, |_, t| t.contains("MCLI-SHELL-PID="));
        id
    }
    fn tab(&self, id: SessionId) -> &crate::workspace::Tab {
        self.rt.state.tabs.iter().find(|t| t.id == id).unwrap()
    }
    fn send(&self, id: SessionId, bytes: &[u8]) {
        self.tab(id).session.as_ref().unwrap().input(bytes);
    }
    fn line(&mut self, id: SessionId, line: &str) {
        self.rt.update(|w, tx| w.select(Some(id), tx));
        self.rt.frame(self.clock.elapsed().as_secs_f64());
        if line.contains('\n') {
            self.send(id, b"\x1b[200~");
            self.send(id, line.as_bytes());
            self.send(id, b"\x1b[201~");
        } else {
            self.send(id, line.as_bytes());
        }
        std::thread::sleep(Duration::from_millis(500));
        self.send(id, b"\r");
    }
    fn until(
        &mut self,
        id: SessionId,
        label: &str,
        seconds: u64,
        check: impl Fn(&crate::workspace::Tab, &str) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            self.rt.frame(self.clock.elapsed().as_secs_f64());
            let tab = self.tab(id);
            let view = tab.session.as_ref().map(frame).unwrap_or_default();
            if check(tab, &view) {
                return;
            }
            if Instant::now() >= deadline || tab.error.is_some() {
                let filtered = view
                    .lines()
                    .filter(|l| !l.contains('@'))
                    .collect::<Vec<_>>()
                    .join("\n");
                std::fs::write(self.root.join(format!("{label}-terminal.txt")), filtered).unwrap();
                panic!(
                    "{label}: agent={:?} run={} id={:?} title={:?} phase={} count={} error={:?} panel={}",
                    tab.foreground,
                    tab.run.is_some(),
                    tab.bound,
                    tab.title,
                    tab.phase,
                    tab.prompt_count,
                    tab.error,
                    self.rt.state.history.error
                )
            }
            std::thread::sleep(Duration::from_millis(140));
        }
    }
    fn launch(&mut self, id: SessionId, command: &str) {
        let launch = if command == "codex" && std::env::var_os("MCLI_TEST_CODEX_SHARED").is_some() {
            r#"codex -c 'tui.terminal_title=["thread-id","thread-name","thread-title"]'"#.to_owned()
        } else if command == "codex" && std::env::var_os("MCLI_TEST_CODEX_LOG_DIR").is_some() {
            let logs = self.root.join(format!("log-{}", id.0));
            std::fs::create_dir_all(&logs).unwrap();
            format!(
                "codex --no-daemon -c 'log_dir=\"{}\"'",
                logs.to_string_lossy()
                    .replace('\\', "/")
                    .replace('\'', "''")
            )
        } else {
            command.to_owned()
        };
        self.line(id, &launch);
        self.until(id, "agent-ready", 70, |t, v| {
            let lower = v.to_lowercase();
            assert!(
                !lower.contains("do you trust")
                    && !lower.contains("trust this directory")
                    && !lower.contains("trust this folder"),
                "测试目录要求确认信任，保留现有设置"
            );
            t.run.is_some()
                && (v.contains("Ask Codex")
                    || v.contains("for shortcuts")
                    || v.contains("Antigravity CLI")
                    || v.contains("esc skip"))
        });
        if self.tab(id).session.as_ref().is_some_and(|s| {
            let f = frame(s);
            f.contains("Update available") && f.contains("esc skip")
        }) {
            self.send(id, b"\x1b");
        }
        std::thread::sleep(Duration::from_millis(650));
    }
    fn prompt(&mut self, id: SessionId, text: &str, count: usize, tag: &str) -> Conversation {
        self.rt.frame(self.clock.elapsed().as_secs_f64());
        assert!(
            self.tab(id).foreground.is_some() && self.tab(id).run.is_some(),
            "{tag}: 官方 CLI 已返回 PowerShell，停止提交测试提问"
        );
        let prompt = format!("Mcli 验收：请只回复 OK，不调用工具，不访问文件。{text}");
        self.line(id, &prompt);
        if self.tab(id).foreground == Some(shell::Foreground::Codex) && !prompt.contains('\n') {
            self.submit_pending_codex_draft(id, &prompt);
        }
        self.until(id, tag, 100, |t, _| {
            t.bound.is_some() && t.prompt_count == count
        });
        let key = self.tab(id).bound.clone().unwrap();
        let store = Store::open(&self.db, Roots::installed().unwrap()).unwrap();
        assert_eq!(
            store.text(&key, count).unwrap(),
            prompt,
            "官方原文与输入不一致"
        );
        self.until(id, &format!("{tag}-completed"), 150, |t, v| {
            matches!(t.state_text().as_str(), "就绪" | "出错" | "已打断")
                || (v.contains("Mode: idle")
                    && (v.contains("FAILED_PRECONDITION") || v.contains("error")))
        });
        let view = frame(self.tab(id).session.as_ref().unwrap());
        self.outcomes.push((
            if view.contains("FAILED_PRECONDITION (code 400)") {
                "provider_error"
            } else {
                "completed"
            },
            view.contains("Mode: idle") || view.contains("Mode: working"),
        ));
        key
    }
    fn press(&mut self, key: &str) {
        press(&mut self.rt, key, self.clock.elapsed().as_secs_f64());
    }
    fn resume(&mut self, tab: SessionId, key: &Conversation) {
        let command = format!("/resume {}", key.id);
        self.line(tab, &command);
        if key.agent == "codex" {
            self.submit_pending_codex_draft(tab, &command);
        }
    }
    fn exit_agent(&mut self, tab: SessionId) {
        let codex = self.tab(tab).foreground == Some(shell::Foreground::Codex);
        self.line(tab, "/exit");
        if codex {
            self.submit_pending_codex_draft(tab, "/exit");
        }
        self.until(tab, "same-powershell", 35, |t, v| {
            t.foreground.is_none() && v.contains("PS ")
        });
    }
    fn submit_pending_codex_draft(&mut self, id: SessionId, expected: &str) {
        for delay in [2, 4, 8] {
            std::thread::sleep(Duration::from_secs(delay));
            self.rt.frame(self.clock.elapsed().as_secs_f64());
            let tab = self.tab(id);
            let view = frame(tab.session.as_ref().unwrap());
            let pending = view
                .lines()
                .rev()
                .take(9)
                .any(|line| line.trim().strip_prefix("› ") == Some(expected));
            if std::env::var_os("MCLI_TEST_HOOK_TRACE").is_some() {
                eprintln!(
                    "Codex 测试输入栏：等待 {delay}s，草稿匹配 {pending}，足迹 {}，运行提示 {}",
                    tab.prompt_count,
                    view.contains("esc to interrupt")
                );
            }
            // During a thread switch Codex can paint the composer before it accepts Enter.
            // Never advance a menu or submit a different draft. No text is sent twice.
            if tab.foreground != Some(shell::Foreground::Codex)
                || view.contains("esc to interrupt")
                || view.contains("Yes, proceed")
                || !pending
            {
                break;
            }
            self.send(id, b"\r");
        }
    }
}

#[test]
#[ignore = "启动本机安装的官方 Codex，在独立配置和回环模型地址下核对 /clear"]
fn official_codex_clear_without_title_id_or_hooks() {
    let (root, roots, db) = super::conversation_index::fixture("native-codex-clear");
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&project)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        roots.codex.join("config.toml"),
        format!(
            r#"
model = "gpt-5.4"
model_provider = "local"
approval_policy = "never"
sandbox_mode = "read-only"
check_for_update_on_startup = false
[model_providers.local]
name = "Local fixture"
base_url = "http://127.0.0.1:1/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
[tui]
terminal_title = ["project-name"]
[projects.'{}']
trust_level = "trusted"
"#,
            project.to_string_lossy().replace('/', "\\").to_lowercase()
        ),
    )
    .unwrap();
    let home = shell_quote(&roots.codex);
    let mut scene = Scene::open_with_roots(project, root, db.clone(), roots.clone());
    let tab = scene.spawn();
    scene.line(tab, &format!("$env:CODEX_HOME = {home}"));
    scene.line(tab, "codex --no-daemon");
    let ready = |view: &str| {
        view.contains("OpenAI Codex") && view.lines().any(|l| l.trim_start().starts_with("› "))
    };
    scene.until(tab, "open-test-project", 40, |_, view| {
        view.contains("Trust this folder?") || ready(view)
    });
    if frame(scene.tab(tab).session.as_ref().unwrap()).contains("Trust this folder?") {
        scene.send(tab, b"\r");
    }
    scene.until(tab, "codex-ready", 40, |_, view| ready(view));
    let submit = |scene: &mut Scene, text| {
        scene.line(tab, text);
        scene.submit_pending_codex_draft(tab, text);
    };
    submit(&mut scene, "first local prompt");
    scene.until(tab, "start-first-thread", 30, |t, v| {
        t.prompt_count == 1 || v.contains("Trust this folder?")
    });
    if scene.tab(tab).prompt_count == 0 {
        scene.send(tab, b"\r");
    }
    scene.until(tab, "old-thread", 25, |t, _| t.prompt_count == 1);
    let old = scene.tab(tab).bound.clone().unwrap();
    scene.press("footprint-toggle");
    super::history_ui::shown(&mut scene.rt, scene.clock, "copy-prompt-1");
    scene.send(tab, b"\x1b");
    scene.until(tab, "first-turn-ended", 25, |t, v| {
        matches!(t.phase.as_str(), "已打断" | "出错" | "就绪")
            || v.contains("Conversation interrupted")
    });
    submit(&mut scene, "/clear");
    scene.until(tab, "fresh-thread", 20, |t, _| {
        t.bound.is_none() && t.preview.is_empty()
    });
    assert!(scene.rt.presented("copy-prompt-1").is_none());
    submit(&mut scene, "new local prompt");
    scene.until(tab, "new-prompt", 30, |t, _| {
        t.bound.as_ref().is_some_and(|k| k != &old) && t.prompt_count == 1
    });
    super::history_ui::shown(&mut scene.rt, scene.clock, "copy-prompt-1");
    let store = Store::open(&db, roots).unwrap();
    assert_eq!(store.text(&old, 1).unwrap(), "first local prompt");
    let second = scene.tab(tab).bound.clone().unwrap();
    assert_eq!(store.text(&second, 1).unwrap(), "new local prompt");
    scene.send(tab, b"\x1b");
    scene.until(tab, "second-turn-ended", 25, |t, v| {
        matches!(t.phase.as_str(), "已打断" | "出错" | "就绪")
            || v.contains("Conversation interrupted")
    });
    submit(&mut scene, "/new");
    scene.until(tab, "choose-workspace", 25, |t, v| {
        t.bound.is_none() || v.contains("Use current Git worktree")
    });
    if scene.tab(tab).bound.is_some() {
        scene.send(tab, b"\r");
    }
    scene.until(tab, "new-thread", 25, |t, _| {
        t.bound.is_none() && t.preview.is_empty()
    });
    submit(&mut scene, "third local prompt");
    scene.until(tab, "third-prompt", 25, |t, _| {
        t.bound.as_ref().is_some_and(|k| k != &old && k != &second) && t.prompt_count == 1
    });
    assert_eq!(
        store
            .text(scene.tab(tab).bound.as_ref().unwrap(), 1)
            .unwrap(),
        "third local prompt"
    );
}
fn workflow(agent: Agent, binary: &str) {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let parent = std::env::var_os("MCLI_QA_WORKFLOW_RESULTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp/phase-three-conversations-and-prompts")
        });
    let root = parent.join(format!("{binary}-{unique}"));
    let project = official_test_project(agent);
    let mut scene = Scene::open(project.clone(), root.clone());
    let a = scene.spawn();
    let b = scene.spawn();
    scene.launch(a, binary);
    scene.launch(b, binary);
    let a1 = scene.prompt(a, "相同提问", 1, "A1-first");
    assert_eq!(a1, scene.prompt(a, "相同提问", 2, "A1-second"));
    assert_eq!(a1, scene.prompt(a, "第一行\n  第二行", 3, "A1-multiline"));
    let b1 = scene.prompt(b, "B 独有", 1, "B1-first");
    assert_ne!(a1, b1);
    scene.line(a, "/new");
    if binary == "codex" {
        scene.until(a, "new-workspace", 30, |_, v| {
            v.contains("Use current Git worktree") || v.contains("New thread started")
        });
        if frame(scene.tab(a).session.as_ref().unwrap()).contains("Use current Git worktree") {
            scene.send(a, b"\r");
        }
    }
    std::thread::sleep(Duration::from_secs(2));
    let a2 = scene.prompt(a, "A2 独有", 1, "A2-first");
    assert_ne!(a1, a2);
    assert_ne!(a2, b1);
    assert_eq!(scene.tab(b).bound.as_ref(), Some(&b1));
    scene.resume(a, &a1);
    scene.until(a, "restore-within-cli", 30, |t, _| {
        t.bound.as_ref() == Some(&a1)
    });
    assert_eq!(a1, scene.prompt(a, "回到 A1", 4, "A1-return"));
    scene.press("footprint-toggle");
    wait_for_workspace(&mut scene.rt, scene.clock, |w| {
        w.current().unwrap().prompt_count == 4
    });
    let store = Store::open(&scene.db, Roots::installed().unwrap()).unwrap();
    assert_eq!(
        [&a1, &b1, &a2].map(|k| store.get(k).unwrap().unwrap().count),
        [4, 1, 1]
    );
    let recent = store.recent().unwrap();
    assert!(
        [&a1, &b1, &a2]
            .into_iter()
            .all(|k| recent.iter().filter(|t| &t.key == k).count() == 1)
    );
    let report = serde_json::json!({"agent":binary,"terminal_a":a.marker(),"terminal_b":b.marker(),"A1":a1.id,"B1":b1.id,"A2":a2.id,"counts":[4,1,1],"turn_outcomes":scene.outcomes});
    drop(store);
    drop(scene);
    let mut reopened = Scene::open(project, root.clone());
    let shell = reopened.spawn();
    let task = format!("task-{binary}-{}", a1.id);
    super::history_ui::shown(&mut reopened.rt, reopened.clock, &task);
    reopened.press(&task);
    let target = reopened.rt.state.active.unwrap();
    assert_ne!(target, shell);
    reopened.until(target, "history-resumed", 65, |t, _| {
        t.bound.as_ref() == Some(&a1) && t.prompt_count == 4
    });
    reopened.exit_agent(target);
    std::fs::write(
        root.join("verified.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
#[test]
#[ignore = "两份已信任的真实 agy、六条合成提问和恢复"]
fn official_phase_three_agy_workflow() {
    workflow(Agent::Agy, "agy");
}

fn check_agy_reply_status(command: &str) {
    let (root, _, _) = super::conversation_index::fixture("agy-reply-ready");
    let mut scene = Scene::open(official_test_project(Agent::Agy), root);
    let id = scene.spawn();
    scene.launch(id, command);
    scene.until(id, "agy-editable-composer", 40, |_, view| {
        view.contains("shift+tab to cycle")
            && view.lines().any(|line| line.trim_start().starts_with("> "))
    });
    for (count, question, answer) in [
        (1, "请只回复 37 乘以 41 的十进制结果，不调用工具。", "1517"),
        (2, "请只回复 29 乘以 31 的十进制结果，不调用工具。", "899"),
    ] {
        scene.send(id, question.as_bytes());
        scene.until(id, "agy-test-draft", 12, |_, view| {
            view.lines()
                .any(|line| line.trim_start().strip_prefix("> ") == Some(question))
        });
        scene.send(id, b"\r");
        scene.until(id, "agy-working", 35, |t, _| {
            t.prompt_count == count && t.state_text() == "运行中" && t.preview == question
        });
        scene.until(id, "agy-arithmetic-reply", 100, |t, screen| {
            if screen.contains("FAILED_PRECONDITION (code 400)") && screen.contains("Error ID:") {
                assert_eq!(t.state_text(), "出错");
                panic!("agy 服务返回 FAILED_PRECONDITION，未生成用于验证就绪状态的回复");
            }
            screen.contains(answer)
        });
        scene.until(id, "agy-reply-ready", 12, |t, _| {
            t.state_text() == "就绪" && t.preview.contains(answer)
        });
        let key = scene.tab(id).bound.as_ref().unwrap();
        let store = Store::open(&scene.db, Roots::installed().unwrap()).unwrap();
        let saved = store.get(key).unwrap().unwrap();
        assert_eq!(saved.phase, "就绪");
        assert_eq!(saved.count, count);
        assert!(saved.preview.contains(answer));
    }
}

#[test]
#[ignore = "真实 agy 采用用户现有状态行，连续两轮核对回复预览与就绪状态"]
fn official_agy_reply_returns_to_ready_with_configured_status_line() {
    check_agy_reply_status("agy");
}

#[test]
#[ignore = "真实 agy 在本次启动关闭状态行，连续两轮核对回复预览与就绪状态"]
fn official_agy_reply_returns_to_ready_without_a_status_command() {
    check_agy_reply_status(r#"agy --settings '{"statusLine":{"enabled":false}}'"#);
}

#[test]
#[ignore = "真实 agy 关闭通知和自定义状态行，检查原生输入区回到空闲后的状态"]
fn official_agy_reply_returns_to_ready_without_notifications() {
    check_agy_reply_status(
        r#"agy --settings '{"notifications":false,"statusLine":{"enabled":false}}'"#,
    );
}

#[test]
#[ignore = "指定尚未信任且没有项目钩子的 agy 测试目录，停在原生信任页面"]
fn official_agy_first_visit_shows_workspace_trust() {
    let project =
        PathBuf::from(std::env::var_os("MCLI_AGY_TEST_PROJECT").expect("指定首次打开的目录"));
    assert!(project.is_dir() && !project.join(".agents/hooks.json").exists());
    let (root, _, _) = super::conversation_index::fixture("native-agy-first-visit");
    let mut scene = Scene::open(project, root);
    let id = scene.spawn();
    scene.line(id, "agy");
    scene.until(id, "native-workspace-trust", 45, |t, view| {
        let message = view.to_lowercase();
        t.prompt_count == 0
            && ["do you trust", "trust this directory", "trust this folder"]
                .iter()
                .any(|phrase| message.contains(phrase))
    });
}

fn verify_native_permission(agent: Agent, command: &str) {
    let (root, _, _) = super::conversation_index::fixture("native-permission");
    let mut scene = Scene::open(official_test_project(agent), root);
    let id = scene.spawn();
    let check_meter =
        command.starts_with("agy") && std::env::var_os("MCLI_QA_AGY_METER_GATE").is_some();
    if check_meter {
        scene.line(id, "$global:McliQaPolicy = @{ toolPermission='request-review'; permissions=@{ask=@('command(*)')}; statusLine=(__McliAgyStatusOptions $env:MCLI_STATUS_HELPER $null) } | ConvertTo-Json -Compress -Depth 5");
        scene.launch(id, "agy --settings $global:McliQaPolicy");
    } else {
        scene.launch(id, command);
    }
    if command.starts_with("agy") {
        scene.until(id, "editable-composer", 45, |_, view| {
            view.lines().any(|l| l.trim_start().starts_with('>'))
                && (view.contains("shift+tab to cycle") || view.contains("Mode: idle"))
        });
    }
    for allow in [false, true] {
        let token = if allow {
            "MCLI-GATE-ALLOW"
        } else {
            "MCLI-GATE-DENY"
        };
        scene.line(id, &format!("{token} Use the shell tool once to run exactly Write-Output (73 * 89). Request one-time permission. Do nothing else."));
        scene.until(id, "native-gate", 60, |t, view| {
            if view.contains("FAILED_PRECONDITION (code 400)") && view.contains("Error ID:") {
                panic!(
                    "官方服务拒绝本次模型请求，未产生审批面板。当前侧栏 {}",
                    t.state_text()
                );
            }
            t.state_text() == "等待用户"
                && view.lines().any(|l| {
                    l.trim()
                        .trim_start_matches(['│', '┃', '$', '>', ' '])
                        .trim()
                        .eq_ignore_ascii_case("Write-Output (73 * 89)")
                })
        });
        if check_meter {
            scene.until(id, "official-meter-waiting", 12, |t, _| {
                t.meter == Some("等待用户")
            });
        }
        if !allow && command.starts_with("codex") {
            let waiting = scene.tab(id).bound.clone().expect("审批应绑定当前对话");
            let other = scene.spawn();
            scene.launch(other, "codex");
            let active = scene.prompt(other, "独立会话继续工作", 1, "sibling-turn");
            assert_ne!(waiting, active);
            assert_eq!(scene.tab(id).bound.as_ref(), Some(&waiting));
            assert_eq!(scene.tab(id).state_text(), "等待用户");
            assert_ne!(scene.tab(other).state_text(), "等待用户");
            let project = format!("project-{}", id.0);
            scene.press(&project);
            assert!(
                scene
                    .rt
                    .state
                    .layout
                    .collapsed
                    .contains(&scene.tab(id).project)
            );
            scene.press(&project);
            scene.rt.frame(0.4);
            scene.press(&format!("session-{}", id.0));
            assert_eq!(scene.rt.state.active, Some(id));
        }
        if command.starts_with("agy") && !allow {
            for _ in 0..3 {
                scene.send(id, b"\x1b[B");
                std::thread::sleep(Duration::from_millis(80));
            }
            scene.until(id, "agy-deny-selected", 4, |_, v| {
                v.lines()
                    .any(|l| l.trim_start().starts_with("> 4. No, cancel"))
            });
            scene.send(id, b"\r");
        } else if allow {
            scene.until(id, "exact-one-time", 4, |_, v| {
                v.lines().any(|l| {
                    let l = l
                        .trim()
                        .trim_start_matches(['>', '›', '❯'])
                        .trim()
                        .to_ascii_lowercase();
                    l.starts_with("1. yes,") && !l.contains("always")
                })
            });
            scene.send(id, b"\r");
        } else {
            scene.send(id, b"\x1b");
        }
        scene.until(id, "permission-resolved", 35, |t, v| {
            t.state_text() != "等待用户"
                && if allow {
                    v.contains("6497")
                } else {
                    let v = v.to_lowercase();
                    !v.contains("6497")
                        && (v.contains("canceled")
                            || v.contains("declined")
                            || v.contains("rejected"))
                }
        });
        if check_meter {
            scene.until(id, "official-meter-resolved", 12, |t, _| {
                matches!(t.meter, Some("运行中" | "就绪" | "在线"))
            });
        }
        if !allow {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}
#[test]
#[ignore = "专用原生 Codex 用已知只读算术命令逐一核对审批、拒绝和同意"]
fn official_codex_workbench_permission() {
    verify_native_permission(
        Agent::Codex,
        "codex --no-daemon -c 'approvals_reviewer=\"user\"' -a on-request -s read-only",
    );
}
#[test]
#[ignore = "已信任目录内的原生 agy 和真实模型，核对一次性算术审批"]
fn official_agy_workbench_permission() {
    verify_native_permission(
        Agent::Agy,
        r#"agy --settings '{"toolPermission":"request-review","permissions":{"ask":["command(*)"]}}'"#,
    );
}

#[test]
#[ignore = "使用受控本机模型的原生 Codex，核对工作、打断、错误和退出"]
fn official_codex_workbench_runtime_states() {
    assert!(
        std::env::var_os("MCLI_QA_STATUS_BACKEND").is_some(),
        "此测试需要受控的本机模型"
    );
    let (root, _, _) = super::conversation_index::fixture("native-codex-states");
    let mut scene = Scene::open(official_test_project(Agent::Codex), root);
    let id = scene.spawn();
    scene.launch(id, "codex");
    scene.prompt(id, "ready", 1, "ready");
    scene.until(id, "native-ready", 16, |t, _| t.state_text() == "就绪");
    scene.line(id, "MCLI-QA-HOLD");
    scene.until(id, "native-working", 20, |t, _| t.state_text() == "运行中");
    scene.send(id, b"\x1b");
    scene.until(id, "native-interrupted", 25, |t, _| {
        t.state_text() == "已打断"
    });
    scene.line(id, "MCLI-QA-FAIL");
    scene.until(id, "native-error", 50, |t, _| t.state_text() == "出错");
    scene.exit_agent(id);
    assert_eq!(scene.tab(id).label(), "PowerShell");
    scene.line(id, "exit");
    scene.until(id, "terminal-exit", 10, |t, _| {
        t.state_text().starts_with("已退出")
    });
}

fn recorded_pair() -> (PathBuf, Agent, Conversation, Conversation) {
    let name = std::env::var("MCLI_QA_PAIR_FOLDER").expect("指定已完成的真实双终端测试目录名");
    let (vendor, digits) = name.split_once('-').unwrap();
    assert!(!digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()));
    let agent = match vendor {
        "codex" => Agent::Codex,
        "agy" => Agent::Agy,
        _ => panic!("未知测试厂商"),
    };
    let base =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp/phase-three-conversations-and-prompts");
    let original = base.join(&name);
    let report = match std::fs::read(original.join("verified.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && agent == Agent::Codex => {
            std::fs::read(original.join("verified-recovery.json"))
        }
        result => result,
    };
    let report: serde_json::Value = serde_json::from_slice(&report.unwrap()).unwrap();
    if let Some(recorded_vendor) = report.get("agent") {
        assert_eq!(recorded_vendor, vendor);
    }
    assert_eq!(report["counts"], serde_json::json!([4, 1, 1]));
    let a1 = Conversation::new(vendor, report["A1"].as_str().unwrap()).unwrap();
    let b1 = Conversation::new(vendor, report["B1"].as_str().unwrap()).unwrap();
    (original, agent, a1, b1)
}

#[test]
#[ignore = "从已完成的三对话验收样本恢复两个官方终端，使用独立剪贴板，仅编辑草稿"]
fn official_drawer_preserves_drafts_and_copies_from_the_selected_thread() {
    let (original, agent, a1, b1) = recorded_pair();
    let base = original.parent().unwrap();
    let name = original.file_name().unwrap().to_str().unwrap();
    let vendor = a1.agent.as_str();
    let root = base.join(format!("drawer-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let database = root.join("conversations.sqlite3");
    let old = rusqlite::Connection::open_with_flags(
        original.join("conversations.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    old.execute("VACUUM INTO ?1", [database.to_str().unwrap()])
        .unwrap();
    drop(old);
    let store = Store::open(&database, Roots::installed().unwrap()).unwrap();
    let a_text = store.text(&a1, 3).unwrap();
    let b_text = store.text(&b1, 1).unwrap();
    assert!(a_text.starts_with("Mcli 验收") && a_text.ends_with("第一行\n  第二行"));
    assert!(b_text.starts_with("Mcli 验收") && b_text.ends_with("B 独有"));
    drop(store);
    let mut scene = Scene::open(official_test_project(agent), root.clone());
    let board = rune::clipboard::memory();
    scene.rt.use_clipboard(&board);
    scene.rt.set_viewport((1120., 420.), 1.);
    scene.spawn();
    let mut ids = Vec::new();
    for (k, count, draft) in [(&a1, 4, "草稿甲314"), (&b1, 1, "草稿乙592")] {
        let task = format!("task-{vendor}-{}", k.id);
        super::history_ui::shown(&mut scene.rt, scene.clock, &task);
        scene.press(&task);
        let id = scene.rt.state.active.unwrap();
        scene.until(id, "drawer-resume", 70, |t, view| {
            let lower = view.to_lowercase();
            let ready = if vendor == "agy" {
                lower.contains("mode: idle") || lower.contains("for shortcuts")
            } else {
                lower.contains("ask codex") || lower.contains("for shortcuts")
            };
            t.bound.as_ref() == Some(k) && t.prompt_count == count && ready
        });
        scene.rt.ime(Ime::Commit(draft.into()));
        scene.until(id, "draft-visible", 12, |_, view| view.contains(draft));
        ids.push(id);
    }
    let mut offsets = Vec::new();
    let mut history_lines = Vec::new();
    let mut alt_screens = Vec::new();
    for (id, other, n, expected, draft) in [
        (ids[0], ids[1], 3, &a_text, "草稿甲314"),
        (ids[1], ids[0], 1, &b_text, "草稿乙592"),
    ] {
        scene.press(&format!("session-{}", id.0));
        scene.until(id, "active-draft", 10, |_, view| view.contains(draft));
        let terminal_size = scene.rt.state.size;
        let s = scene.rt.state.session().unwrap();
        let digits = &draft[draft.len() - 3..];
        let (row, col) = {
            use alacritty_terminal::grid::Dimensions;
            use alacritty_terminal::index::Line;
            let term = s.term.lock().unwrap();
            (0..term.screen_lines())
                .rev()
                .find_map(|y| {
                    let line = Line(y as i32 - term.grid().display_offset() as i32);
                    term.grid()[line][..]
                        .windows(3)
                        .position(|cells| cells.iter().map(|c| c.c).eq(digits.chars()))
                        .map(|x| (y as u16, x as u16))
                })
                .expect("定位草稿末尾的合成数字")
        };
        s.select(col, row, false, Some(SelectionType::Simple));
        s.select(col + 2, row, true, None);
        let (before, available) = {
            let term = s.term.lock().unwrap();
            alt_screens.push(term.mode().contains(TermMode::ALT_SCREEN));
            (term.grid().display_offset(), term.grid().history_size())
        };
        s.scroll(1);
        let offset = s.term.lock().unwrap().grid().display_offset();
        assert_eq!(offset, (before + 1).min(available));
        offsets.push(offset);
        history_lines.push(available);
        assert_eq!(s.selected().as_deref(), Some(digits));
        scene.press("footprint-toggle");
        let copy = format!("copy-prompt-{n}");
        super::history_ui::shown(&mut scene.rt, scene.clock, &copy);
        assert_eq!(scene.rt.state.size, terminal_size);
        scene.press(&format!("prompt-{n}"));
        scene.press(&copy);
        let deadline = Instant::now() + Duration::from_secs(8);
        while board.text().as_deref().map(|s| s.as_str()) != Some(expected.as_str()) {
            scene.rt.frame(0.016);
            assert!(Instant::now() < deadline, "抽屉复制内容与官方记录不一致");
            std::thread::sleep(Duration::from_millis(15));
        }
        scene.press("close-footprints");
        assert_eq!(scene.rt.state.size, terminal_size);
        let s = scene.rt.state.session().unwrap();
        assert_eq!(
            s.selected().as_deref(),
            Some(digits),
            "原选区第 {row} 行、第 {col} 列，向上 {offset}/{available} 行"
        );
        assert_eq!(s.term.lock().unwrap().grid().display_offset(), offset);
        s.scroll(-(offset as i32));
        scene.rt.key(KeyEvent {
            key: Key::Text("q".into()),
            mods: Mods::default(),
        });
        let final_draft = format!("{draft}q");
        scene.until(id, "focus-at-draft", 10, |_, view| {
            view.contains(&final_draft)
        });
        assert!(!frame(scene.tab(other).session.as_ref().unwrap()).contains(&final_draft));
    }
    assert_eq!(scene.tab(ids[0]).prompt_count, 4);
    assert_eq!(scene.tab(ids[1]).prompt_count, 1);
    std::fs::write(root.join("verified-drawer.json"), serde_json::to_vec(&serde_json::json!({
        "vendor":vendor,"a1_count":4,"b1_count":1,"a1_multiline_copy":true,"b1_distinct_copy":true,
        "native_drafts_preserved":2,"native_selections_preserved":2,"scrollback_offsets":offsets,
        "history_lines":history_lines,"alternate_screens":alt_screens,
        "copy_backend":"isolated memory","windows_scale":1.0
    })).unwrap()).unwrap();
}
#[test]
#[ignore = "两份已信任的真实 Codex、六条合成提问和恢复"]
fn official_phase_three_codex_workflow() {
    workflow(Agent::Codex, "codex");
}

fn wait_for_window(path: &Path) {
    let limit = Instant::now() + Duration::from_secs(90);
    while !path.is_file() {
        assert!(
            Instant::now() < limit,
            "等待另一窗口超时 {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(40));
    }
}
fn meet_other_window(folder: &Path, phase: &str, peer: usize) {
    std::fs::write(folder.join(format!("{phase}-{peer}")), []).unwrap();
    wait_for_window(&folder.join(format!("{phase}-{}", 1 - peer)));
}

fn check_shared_window(folder: &Path, peer: usize, agent: Agent, key: &Conversation) {
    let root = folder.join(format!("window-{peer}"));
    let db = folder.join("conversations.sqlite3");
    meet_other_window(folder, "start", peer);
    let mut scene = Scene::open_with_database(official_test_project(agent), root, db);
    let board = rune::clipboard::memory();
    scene.rt.use_clipboard(&board);
    scene.spawn();
    let task = format!("task-{}-{}", key.agent, key.id);
    super::history_ui::shown(&mut scene.rt, scene.clock, &task);
    let second_codex = agent == Agent::Codex && peer == 1;
    if second_codex {
        wait_for_window(&folder.join("owner-ready"));
    }
    scene.press(&task);
    let tab = scene.rt.state.active.unwrap();
    scene.until(tab, "shared-resume", 80, |t, view| {
        let view = view.to_lowercase();
        let ready = if second_codex {
            view.contains("this conversation is open in another app")
        } else {
            view.contains("for shortcuts")
                || view.contains("mode: idle")
                || view.contains("ask codex")
        };
        t.bound.as_ref() == Some(key) && t.prompt_count == 4 && ready
    });
    let drafts = ["窗口甲草稿318", "窗口乙草稿572"];
    let draft = drafts[peer];
    if !second_codex {
        scene.rt.ime(Ime::Commit(draft.into()));
        scene.until(tab, "draft", 12, |_, view| view.contains(draft));
        if agent == Agent::Codex {
            std::fs::write(folder.join("owner-ready"), []).unwrap();
        }
    }
    meet_other_window(folder, "viewing", peer);
    scene.press("footprint-toggle");
    super::history_ui::shown(&mut scene.rt, scene.clock, "copy-prompt-3");
    scene.press("copy-prompt-3");
    let store = Store::open(&scene.db, Roots::installed().unwrap()).unwrap();
    let text = store.text(key, 3).unwrap();
    assert!(text.starts_with("Mcli 验收") && text.ends_with("第一行\n  第二行"));
    scene.until(tab, "copy", 8, |_, _| {
        board.text().as_deref().map(|t| t.as_str()) == Some(text.as_str())
    });
    scene.press("close-footprints");
    meet_other_window(folder, "copied", peer);
    if second_codex {
        wait_for_window(&folder.join("owner-released"));
        scene.rt.key(KeyEvent {
            key: Key::Text("r".into()),
            mods: Mods::default(),
        });
        scene.until(tab, "native-retry", 45, |t, view| {
            let view = view.to_lowercase();
            t.bound.as_ref() == Some(key)
                && !view.contains("this conversation is open in another app")
                && (view.contains("for shortcuts") || view.contains("ask codex"))
        });
        scene.rt.ime(Ime::Commit(draft.into()));
        scene.until(tab, "draft-after-release", 12, |_, view| {
            view.contains(draft)
        });
    }
    scene.rt.key(KeyEvent {
        key: Key::Text("q".into()),
        mods: Mods::default(),
    });
    scene.until(tab, "continued-draft", 12, |_, view| {
        view.contains(&format!("{draft}q"))
    });
    scene.rt.frame(scene.clock.elapsed().as_secs_f64());
    assert!(!frame(scene.tab(tab).session.as_ref().unwrap()).contains(drafts[1 - peer]));
    let result = serde_json::json!({"terminal":tab.marker(),"launch":scene.tab(tab).run,
        "thread":key.id,"draft":format!("{draft}q"),"foreign_draft_seen":false,
        "prompts":store.get(key).unwrap().unwrap().count,"third_prompt_copy":true,
        "native_read_only_while_owned":second_codex,"edited_after_native_retry":second_codex});
    if agent == Agent::Codex && !second_codex {
        scene.rt.update(|w, tx| w.close(tab, tx));
        wait_for_workspace(&mut scene.rt, scene.clock, |w| {
            w.tabs.iter().all(|t| t.id != tab)
        });
        std::fs::write(folder.join("owner-released"), []).unwrap();
    }
    std::fs::write(
        folder.join(format!("result-{peer}.json")),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "两个独立宿主进程共用专用索引，各自在官方 CLI 恢复同一对话并保留不同草稿"]
fn official_two_workspaces_share_one_index() {
    const GROUP: &str = "MCLI_QA_HOST_PAIR_PID";
    const PEER: &str = "MCLI_QA_HOST_PAIR_PEER";
    let (original, agent, key, _) = recorded_pair();
    let parent = std::env::var(GROUP).ok().map(|p| p.parse::<u32>().unwrap());
    let name = original.file_name().unwrap().to_str().unwrap();
    let folder = original.with_file_name(format!(
        "two-workspaces-{name}-{}",
        parent.unwrap_or(std::process::id())
    ));
    if parent.is_some() {
        let peer = std::env::var(PEER).unwrap().parse::<usize>().unwrap();
        assert!(peer < 2);
        check_shared_window(&folder, peer, agent, &key);
        return;
    }
    std::fs::create_dir(&folder).unwrap();
    let db = folder.join("conversations.sqlite3");
    drop(Store::open(&db, Roots::installed().unwrap()).unwrap());
    let original_db = rusqlite::Connection::open_with_flags(
        original.join("conversations.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (project, source, count, sample): (String,String,usize,String) = original_db.query_row(
        "SELECT c.project,c.source,c.prompt_count,p.summary FROM conversations c JOIN prompts p USING(agent,id) WHERE c.agent=? AND c.id=? AND p.ordinal=1",
        rusqlite::params![key.agent,key.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(count, 4);
    assert!(sample.starts_with("Mcli 验收"));
    // Only the official locator exists initially. Both workers must catch up from byte zero.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "INSERT INTO conversations(agent,id,project,source) VALUES(?,?,?,?)",
            rusqlite::params![key.agent, key.id, project, source],
        )
        .unwrap();
    let mut children = Vec::new();
    for peer in 0..2 {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "tests::conversation_workflow::official_two_workspaces_share_one_index",
                    "--nocapture",
                ])
                .env(GROUP, std::process::id().to_string())
                .env(PEER, peer.to_string())
                .env_remove("MCLI_TEST_HOOK_TRACE")
                .spawn()
                .unwrap(),
        );
    }
    let statuses: Vec<_> = children.iter_mut().map(|c| c.wait().unwrap()).collect();
    assert!(
        statuses.iter().all(|s| s.success()),
        "双窗口退出状态 {statuses:?}"
    );
    let outcomes: Vec<serde_json::Value> = (0..2)
        .map(|n| {
            serde_json::from_slice(&std::fs::read(folder.join(format!("result-{n}.json"))).unwrap())
                .unwrap()
        })
        .collect();
    assert_ne!(outcomes[0]["terminal"], outcomes[1]["terminal"]);
    assert_ne!(outcomes[0]["launch"], outcomes[1]["launch"]);
    assert_ne!(outcomes[0]["draft"], outcomes[1]["draft"]);
    let store = Store::open(&db, Roots::installed().unwrap()).unwrap();
    assert_eq!(store.recent().unwrap().len(), 1);
    assert_eq!(store.page(&key, 0).unwrap().len(), 4);
    assert_eq!(store.text(&key, 1).unwrap(), store.text(&key, 2).unwrap());
    assert!(store.text(&key, 4).unwrap().ends_with("回到 A1"));
    std::fs::write(
        folder.join("verified.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
        "agent":key.agent,"official_id":key.id,"before":0,"after":4,"recent":1,"windows":outcomes}))
        .unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "从本次双终端试验的专用索引继续核对 Codex 内部恢复、第四条提问和重开"]
fn official_phase_three_codex_restore_recorded_pair() {
    let name = std::env::var("MCLI_QA_PAIR_FOLDER").expect("指定本轮双终端测试的目录名");
    assert!(
        name.strip_prefix("codex-")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/phase-three-conversations-and-prompts")
        .join(name);
    let dir = official_test_project(Agent::Codex);
    let store = Store::open(
        &root.join("conversations.sqlite3"),
        Roots::installed().unwrap(),
    )
    .unwrap();
    let items = store.recent().unwrap();
    assert_eq!(items.len(), 3);
    let a1 = items
        .iter()
        .find(|s| {
            (3..=4).contains(&s.count) && store.text(&s.key, 1).unwrap().ends_with("相同提问")
        })
        .expect("A1 的已核对提问")
        .key
        .clone();
    let a2 = items
        .iter()
        .find(|s| store.text(&s.key, 1).unwrap().ends_with("A2 独有"))
        .unwrap()
        .key
        .clone();
    let b1 = items
        .iter()
        .find(|s| store.text(&s.key, 1).unwrap().ends_with("B 独有"))
        .unwrap()
        .key
        .clone();
    assert!(
        store
            .text(&a1, 1)
            .unwrap()
            .starts_with("Mcli 验收：请只回复 OK")
    );
    let prior_count = store.get(&a1).unwrap().unwrap().count;
    if prior_count == 4 {
        assert!(store.text(&a1, 4).unwrap().ends_with("回到 A1"));
    }
    drop(store);
    for (pass, key, count) in [(0, &a2, 1), (1, &a1, 4)] {
        let mut scene = Scene::open(dir.clone(), root.clone());
        let shell = scene.spawn();
        scene.until(shell, "recent-loaded", 20, |_, s| {
            s.contains("MCLI-SHELL-PID=")
        });
        let task = format!("task-codex-{}", key.id);
        super::history_ui::shown(&mut scene.rt, scene.clock, &task);
        scene.press(&task);
        let id = scene.rt.state.active.unwrap();
        assert_ne!(shell, id);
        scene.until(id, "history-official-id", 70, |t, v| {
            t.bound.as_ref() == Some(key)
                && t.prompt_count == count
                && (v.contains("Ask Codex") || v.contains("for shortcuts"))
        });
        if pass == 0 {
            scene.resume(id, &a1);
            scene.until(id, "within-cli-official-id", 35, |t, _| {
                t.bound.as_ref() == Some(&a1) && t.prompt_count == prior_count
            });
            if prior_count == 3 {
                assert_eq!(scene.prompt(id, "回到 A1", 4, "A1-return"), a1);
            }
        }
        scene.exit_agent(id);
    }
    let store = Store::open(
        &root.join("conversations.sqlite3"),
        Roots::installed().unwrap(),
    )
    .unwrap();
    assert_eq!(
        [&a1, &b1, &a2].map(|k| store.get(k).unwrap().unwrap().count),
        [4, 1, 1]
    );
    std::fs::write(
        root.join("verified-recovery.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"A1":a1.id,"B1":b1.id,"A2":a2.id,"counts":[4,1,1]}),
        )
        .unwrap(),
    )
    .unwrap();
}
