use super::conversation_index::{key, prompt, put, source};
use super::*;
use crate::{conversation_store::Store, history::Service, transcript::Roots};
use serde_json::{Value, json};
use std::{fs, process::Command};

// Log comparisons, never the process environment. Both fixture CLIs use their
// native console and the same ID number in different provider namespaces.
const PROBE: &str = r#"
$vendor = $args[0]
$slot = if ($vendor -eq 'codex') { 'A' } else { 'B' }
$port = if ($slot -eq 'A') { 29101 } else { 29102 }
$id = '01234567-1234-4321-8765-000000000001'
$stage = $env:MCLI_PROXY_QA_STEP
$tab = $env:MCLI_SESSION_ID
$run = $env:MCLI_LAUNCH_ID
$checks = @{
    http=($env:HTTP_PROXY -ceq "http://127.0.0.1:$port")
    https=($env:HTTPS_PROXY -ceq "http://qa-$slot@127.0.0.1:$port")
    all=($null -eq [Environment]::GetEnvironmentVariable('ALL_PROXY'))
    bypass=($env:NO_PROXY -ceq "$slot.invalid")
    unrelated=(($stage -eq 'save') -eq ($env:MCLI_UNRELATED_QA -ceq 'temporary'))
    private=(!$env:MCLI_PROXY_DIR)
    tty=(![Console]::IsInputRedirected -and ![Console]::IsOutputRedirected)
    identity=($stage -eq 'save' -or $args -contains $id)
}
$log = "$stage-$vendor-$run.json"
[IO.File]::WriteAllText((Join-Path $pwd $log), ($checks | ConvertTo-Json -Compress))
if ($stage -ne 'save' -and !(Test-Path "$stage-$tab.attempt")) {
    [IO.File]::WriteAllText((Join-Path $pwd "$stage-$tab.attempt"), '1')
    exit 75
}
if ($vendor -eq 'codex') { [Console]::Title = $id.Substring(0,29) + '... | ' + $id }
else {
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', "$tab-$run-agy", 'Out')
    $pipe.Connect(2000)
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes("Starting conversation update stream for $id" + [char]10)
        $pipe.Write($bytes,0,$bytes.Length)
    } finally { $pipe.Dispose() }
}
[Console]::WriteLine('QA-ATTACHED')
$null = [Console]::ReadLine()
"#;

#[test]
fn both_powershells_follow_new_profile_defaults_and_keep_explicit_removals() {
    let root = phase_four::folder("profile-proxy");
    let legacy = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    for (n, exe) in [pwsh(), legacy]
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_file())
    {
        let dir = root.join(n.to_string());
        fs::create_dir(&dir).unwrap();
        let current = dir.join("current");
        fs::write(&current, "").unwrap();
        let before = r#"
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$phase=$env:MCLI_QA_PROFILE
$env:HTTP_PROXY="http://127.0.0.1:$phase"
$env:NO_PROXY="profile-$phase.invalid"
$env:ALL_PROXY=if ($phase -ne '22002') { 'http://127.0.0.1:23000' } else { $null }
"#;
        let after = r#"
if ($phase -eq '22005') {
    if (!$global:__McliProxyFailed) { exit 7 }
    function __McliAgentReport($report) { $global:notice=$report }
    function codex { exit 8 }
    __McliRestore 'codex' 'codex'
    if ($global:notice -ne 'resume-unavailable') { exit 9 }
    exit 0
}
if ($global:__McliProxyFailed) { exit 1 }
if ($phase -eq '22001') {
    $env:HTTP_PROXY='http://127.0.0.1:22123'
    Remove-Item Env:ALL_PROXY
    __McliCaptureProxy
} elseif ($phase -eq '22004') {
    if ($env:HTTP_PROXY -cne 'http://127.0.0.1:22004' -or !$env:ALL_PROXY) { exit 2 }
} else {
    if ($env:HTTP_PROXY -cne 'http://127.0.0.1:22123' -or
        $null -ne [Environment]::GetEnvironmentVariable('ALL_PROXY') -or
        $env:NO_PROXY -cne "profile-$phase.invalid") { exit 3 }
    if ($phase -eq '22003') {
        $env:HTTP_PROXY="http://127.0.0.1:$phase"
        $env:ALL_PROXY='http://127.0.0.1:23000'
    }
    __McliCaptureProxy
}
"#;
        let driver = format!(
            "{before}\n{}\n{}\n{after}",
            include_str!("../shell_proxy.ps1"),
            include_str!("../shell_restore.ps1")
        );
        for phase in 22001..=22005 {
            if phase == 22005 {
                fs::write(&current, "AAAA").unwrap();
            }
            let result = Command::new(exe)
                .args(["-NoProfile", "-NonInteractive", "-Command"])
                .arg(&driver)
                .env("MCLI_PROXY_DIR", &dir)
                .env("MCLI_QA_PROFILE", phase.to_string())
                .env("HTTPS_PROXY", "http://127.0.0.1:22900")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "PowerShell {n}, phase {phase}, exit {:?}: {}",
                result.status.code(),
                String::from_utf8_lossy(&result.stderr)
            );
            let saved = fs::read_to_string(&current).unwrap();
            match phase {
                22001 | 22002 => assert!(saved.len() > 80 && !saved.contains("127.0.0.1")),
                22003 | 22004 => assert!(saved.is_empty()),
                22005 => assert_eq!(saved, "AAAA"),
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn independent_proxies_follow_tabs_history_and_each_retry() {
    let Some(root) = std::env::var_os("MCLI_PROXY_QA_ROOT") else {
        let root = phase_four::folder("session-proxy");
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("probe.ps1"), PROBE).unwrap();
        for name in ["codex", "agy"] {
            fs::write(bin.join(format!("{name}.cmd")), format!("@echo off\r\npwsh.exe -NoProfile -File \"%~dp0probe.ps1\" {name} %*\r\nexit /b %errorlevel%\r\n")).unwrap();
        }
        let path = std::env::join_paths([
            bin,
            pwsh().parent().unwrap().to_owned(),
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"),
        ])
        .unwrap();
        for stage in ["save", "reopen", "history"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::session_proxy::independent_proxies_follow_tabs_history_and_each_retry",
                    "--nocapture",
                ])
                .env("MCLI_PROXY_QA_ROOT", &root)
                .env("MCLI_PROXY_QA_STEP", stage)
                .env("PATH", &path)
                .env("HTTP_PROXY", "http://127.0.0.1:29000")
                .env("HTTPS_PROXY", "http://127.0.0.1:29001")
                .env("ALL_PROXY", "http://127.0.0.1:29002")
                .env("NO_PROXY", format!("{stage}.invalid"))
                .env_remove("MCLI_UNRELATED_QA")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{stage}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let root = PathBuf::from(root);
    let stage = std::env::var("MCLI_PROXY_QA_STEP").unwrap();
    let slots = root.join("windows");
    let project = root.join("共享 项目");
    let db = root.join("index.sqlite3");
    let roots = Roots {
        codex: root.join("codex"),
        agy: root.join("agy"),
        kimi: root.join("kimi"),
        grok: root.join("grok"),
    };
    let keys = [key("codex", 1), key("agy", 1)];
    if stage == "save" {
        fs::create_dir(&project).unwrap();
        let mut store = Store::open(&db, roots.clone()).unwrap();
        for (i, k) in keys.iter().enumerate() {
            let file = source(&roots, k);
            if k.agent == "codex" {
                put(
                    &file,
                    json!({"type":"session_meta","payload":{"id":k.id,"cwd":project}}),
                );
            }
            put(
                &file,
                prompt(k, 1, "proxy fixture", Some("2026-11-22T08:00:00Z")),
            );
            while store.sync(k, &project, &file).unwrap() {}
            let slot = ["A", "B"][i];
            let port = 29101 + i;
            fs::write(project.join(format!("set-{slot}.ps1")), format!("$env:http_proxy='http://127.0.0.1:{port}'\n$env:HTTPS_PROXY='http://qa-{slot}@127.0.0.1:{port}'\n$env:NO_PROXY='{slot}.invalid'\nRemove-Item Env:ALL_PROXY -ErrorAction SilentlyContinue\n$env:MCLI_UNRELATED_QA='temporary'\n")).unwrap();
        }
    }
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1120., 720.), rune::Env::new(true, 1.));
    rt.update(|w, tx| {
        w.enable_restore(&slots);
        w.history.service = Some(Service::start(db, roots, w.wake.clone()));
        if stage == "save" {
            for _ in 0..2 {
                w.new_shell(Some(project.clone()), tx);
            }
        }
    });
    if stage == "save" {
        wait_for_workspace(&mut rt, clock, |w| {
            w.tabs.iter().all(|t| t.session.is_some())
        });
        for (t, slot) in rt.state.tabs.iter().zip(["A", "B"]) {
            t.session
                .as_ref()
                .unwrap()
                .input(format!(". .\\set-{slot}.ps1\r").as_bytes());
        }
        wait_for_workspace(&mut rt, clock, |w| {
            w.tabs.iter().all(|t| {
                t.proxy
                    .as_ref()
                    .unwrap()
                    .snapshot()
                    .unwrap()
                    .is_some_and(|v| !v.is_empty())
            })
        });
        for (t, k) in rt.state.tabs.iter().zip(&keys) {
            t.session
                .as_ref()
                .unwrap()
                .input(format!("{}\r", k.agent).as_bytes());
        }
    } else if stage == "history" {
        for k in &keys {
            let row = format!("task-{}-{}", k.agent, k.id);
            loop {
                rt.frame(clock.elapsed().as_secs_f64());
                if rt.presented(&row).is_some() {
                    break;
                }
                assert!(clock.elapsed() < Duration::from_secs(12), "history row");
                std::thread::sleep(Duration::from_millis(25));
            }
            press(&mut rt, &row, clock.elapsed().as_secs_f64());
        }
    }
    wait_for_workspace(&mut rt, clock, |w| {
        w.tabs.len() == 2
            && w.tabs
                .iter()
                .zip(&keys)
                .all(|(t, k)| t.bound.as_ref() == Some(k))
    });
    for k in &keys {
        let prefix = format!("{stage}-{}-", k.agent);
        let logs: Vec<_> = fs::read_dir(&project)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .collect();
        assert_eq!(logs.len(), if stage == "save" { 1 } else { 2 });
        for entry in logs {
            let checks: Value = serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
            assert!(
                checks.as_object().unwrap().values().all(|v| v == true),
                "{stage} {} {checks}",
                k.agent
            );
        }
    }
    if stage == "reopen" {
        let ids: Vec<_> = rt.state.tabs.iter().map(|t| t.id).collect();
        rt.update(|w, tx| {
            for id in ids {
                w.close(id, tx)
            }
        });
    }
    rt.update(Workspace::save_window);
    if stage == "save" {
        let snapshot = fs::read_to_string(slots.join("1.json")).unwrap();
        assert!(!snapshot.contains("127.0.0.1") && !snapshot.contains("qa-A"));
        let saved: Vec<crate::workspace_file::SavedTab> = serde_json::from_value(
            serde_json::from_str::<Value>(&snapshot).unwrap()["tabs"].clone(),
        )
        .unwrap();
        assert_ne!(saved[0].proxy, saved[1].proxy);
        for (t, k) in saved.into_iter().zip(&keys) {
            let sealed = fs::read_to_string(
                slots
                    .join("proxy-history")
                    .join(format!("{}-{}.bin", k.agent, k.id)),
            )
            .unwrap();
            assert!(sealed.len() > 80 && t.proxy.as_deref() == Some(sealed.as_str()));
        }
    }
}
