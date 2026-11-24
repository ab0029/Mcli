use super::conversation_index::{key, prompt, put, source};
use super::*;
use crate::conversation_store::Store;
use crate::history::Service;
use crate::transcript::Roots;
use serde_json::{Value, json};
use std::{fs, process::Command};

// The child test process resolves only the fixture CLIs. Both are native .cmd
// launchers with a real console, including on window restore via ShellLaunch.
const PROBE: &str = r#"
$id = $args | Where-Object { $_ -match '^[a-f0-9-]{36}$' } | Select-Object -Last 1
$record = @{ id=$id; args=$args; run=$env:MCLI_LAUNCH_ID; tab=$env:MCLI_SESSION_ID;
    cwd=$pwd.Path; ms=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds();
    tty=(![Console]::IsInputRedirected -and ![Console]::IsOutputRedirected -and ![Console]::IsErrorRedirected) }
$log = Join-Path $pwd 'attempts.jsonl'
[IO.File]::AppendAllText($log, (($record | ConvertTo-Json -Compress) + [Environment]::NewLine))
if (!$id) { [Console]::WriteLine('MANUAL-EXIT'); exit 75 }
if (Test-Path 'empty-return') { [Console]::Error.WriteLine('Connection timed out'); exit 0 }
$n = [IO.File]::ReadAllLines($log).Length
if ($n -eq 1) { [Console]::Error.WriteLine('Connection reset'); exit 75 }
[Console]::Title = $id.Substring(0,29) + '... | ' + $id
[Console]::WriteLine("RESTORED-$n")
$end = [DateTime]::UtcNow.AddSeconds(35)
while (!(Test-Path "finish-$n") -and [DateTime]::UtcNow -lt $end) { [Threading.Thread]::Sleep(20) }
if ($n -eq 2) { [Console]::Error.WriteLine('Connection reset after handshake'); exit 75 }
exit 0
"#;

fn attempts(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("attempts.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn verify_attempts(rows: &[Value], dir: &Path, tab: SessionId, id: &str, flag: &str) {
    let mut runs = std::collections::HashSet::new();
    for row in rows {
        assert_eq!(row["tab"], tab.marker());
        assert_eq!(Path::new(row["cwd"].as_str().unwrap()), dir);
        assert_eq!(row["id"], id);
        assert_eq!(row["tty"], true);
        assert!(
            row["args"]
                .as_array()
                .unwrap()
                .windows(2)
                .any(|pair| pair == [flag, id])
        );
        let run = row["run"].as_str().unwrap();
        assert!(crate::hook_wire::valid_run(run) && runs.insert(run));
    }
    for (i, pair) in rows.windows(2).enumerate() {
        let elapsed = pair[1]["ms"].as_i64().unwrap() - pair[0]["ms"].as_i64().unwrap();
        assert!(
            elapsed >= (1000 << i) - 50,
            "retry {i} started after {elapsed} ms"
        );
    }
}

#[test]
fn window_and_history_retry_the_exact_session_and_leave_a_usable_shell() {
    let Some(directory) = std::env::var_os("MCLI_RESTORE_RETRY_FIXTURE") else {
        let root = phase_four::folder("restore-retry");
        let bin = root.join("fixture-bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("probe.ps1"), PROBE).unwrap();
        for exe in ["codex.cmd", "agy.cmd"] {
            fs::write(bin.join(exe), b"@echo off\r\npwsh.exe -NoProfile -File \"%~dp0probe.ps1\" %*\r\nexit /b %errorlevel%\r\n").unwrap();
        }
        let paths = [
            bin,
            pwsh().parent().unwrap().to_owned(),
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"),
        ];
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::restore_retry::window_and_history_retry_the_exact_session_and_leave_a_usable_shell", "--nocapture"])
            .env("MCLI_RESTORE_RETRY_FIXTURE", &root)
            .env("PATH", std::env::join_paths(paths).unwrap()).output().unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    };
    let root = PathBuf::from(directory);
    let codex_dir = root.join("Codex 项目");
    let agy_dir = root.join("agy 项目");
    let slots = root.join("windows");
    for dir in [&codex_dir, &agy_dir, &slots] {
        fs::create_dir_all(dir).unwrap();
    }
    let roots = Roots {
        codex: root.join("records/codex"),
        agy: root.join("records/agy"),
        kimi: root.join("records/kimi"),
        grok: root.join("records/grok"),
    };
    let db = root.join("index.sqlite3");
    let codex = key("codex", 1);
    let agy = key("agy", 2);
    {
        let mut store = Store::open(&db, roots.clone()).unwrap();
        for (key, dir) in [(&codex, &codex_dir), (&agy, &agy_dir)] {
            let file = source(&roots, key);
            if key == &codex {
                put(
                    &file,
                    json!({"type":"session_meta","payload":{"id":key.id,"cwd":dir}}),
                );
            }
            put(
                &file,
                prompt(key, 1, "restore fixture", Some("2026-11-22T08:00:00Z")),
            );
            while store.sync(key, dir, &file).unwrap() {}
        }
    }
    fs::write(slots.join("1.json"), json!({"version":1, "tabs":[{"directory":&codex_dir, "conversation":&codex}], "selected":0, "collapsed":[]}).to_string()).unwrap();
    fs::write(agy_dir.join("empty-return"), "").unwrap();
    let mut rt = app().headless_runtime((1120., 720.), rune::Env::new(true, 1.));
    rt.update(|w, _| {
        w.enable_restore(&slots);
        w.history.service = Some(Service::start(db, roots, w.wake.clone()));
    });
    let clock = Instant::now();
    // The second attempt advertises the real ID but loses its connection before user input.
    // The third leaves normally with the same ID and must stay closed.
    for n in [2, 3] {
        wait_for_workspace(&mut rt, clock, |w| {
            w.current().is_some_and(|t| {
                t.bound.as_ref() == Some(&codex)
                    && t.session
                        .as_ref()
                        .is_some_and(|s| frame(s).contains(&format!("RESTORED-{n}")))
            })
        });
        fs::write(codex_dir.join(format!("finish-{n}")), "").unwrap();
    }
    wait_for_workspace(&mut rt, clock, |w| {
        w.current()
            .is_some_and(|t| t.expected.is_none() && t.foreground.is_none())
    });
    assert_eq!(rt.state.tabs.len(), 1);
    let codex_tab = rt.state.active.unwrap();
    let log = attempts(&codex_dir);
    assert_eq!(log.len(), 3);
    verify_attempts(&log, &codex_dir, codex_tab, &codex.id, "resume");

    let row = format!("task-agy-{}", agy.id);
    press(&mut rt, &row, clock.elapsed().as_secs_f64());
    let agy_tab = rt.state.active.unwrap();
    wait_for_workspace(&mut rt, clock, |w| {
        w.session().is_some_and(|s| frame(s).contains("1/3"))
    });
    press(&mut rt, &row, clock.elapsed().as_secs_f64());
    assert_eq!(rt.state.active, Some(agy_tab));
    assert_eq!(rt.state.tabs.len(), 2);
    wait_for_workspace(&mut rt, clock, |w| w.history.error.contains("已重试 3 次"));
    let log = attempts(&agy_dir);
    assert_eq!(log.len(), 4);
    verify_attempts(&log, &agy_dir, agy_tab, &agy.id, "--conversation");
    assert!(frame(rt.state.session().unwrap()).contains("已重试 3 次"));

    // An explicit later restore gets its own budget. Ctrl+C cancels the first
    // delay; plain commands afterwards still belong to this same PowerShell.
    press(&mut rt, &row, clock.elapsed().as_secs_f64());
    let new_tab = rt.state.active.unwrap();
    assert_ne!(new_tab, agy_tab);
    wait_for_workspace(&mut rt, clock, |w| {
        w.session().is_some_and(|s| frame(s).contains("1/3"))
    });
    let interrupt = || KeyEvent {
        key: Key::Command('c'),
        mods: Mods {
            ctrl: true,
            ..Default::default()
        },
    };
    rt.key(interrupt());
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().is_some_and(|t| t.expected.is_none())
    });
    let session = rt.state.session().unwrap();
    session.input(b"agy\r");
    wait_for(session, 12, || frame(session).contains("MANUAL-EXIT"));
    session.input(b"[Console]::WriteLine(('SHELL-OK-' + 42))\r");
    wait_for(session, 5, || frame(session).contains("SHELL-OK-42"));
    let log = attempts(&agy_dir);
    assert_eq!(log.len(), 6);
    assert_eq!(log[4]["tab"], new_tab.marker());
    assert!(log[5]["id"].is_null());
    assert_eq!(attempts(&codex_dir).len(), 3);

    let legacy = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if legacy.is_file() {
        let dir = root.join("PS5");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("empty-return"), "").unwrap();
        let mut args = shell::arguments_for(Some(&agy));
        args.insert(0, "-NoProfile".into());
        let session =
            Session::spawn(&legacy, &args, &dir, Size::default(), Arc::new(|| {})).unwrap();
        wait_for(&session, 18, || frame(&session).contains("2/3"));
        let key = interrupt();
        session.key_input(&key, input::key(&key, TermMode::REPORT_ALL_KEYS_AS_ESC));
        wait_for(&session, 5, || {
            session.listener.shell.lock().unwrap().restore_end == Some(shell::RestoreEnd::Finished)
        });
        assert_eq!(attempts(&dir).len(), 2);
    }
}
