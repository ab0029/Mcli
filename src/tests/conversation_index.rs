use crate::conversation_store::{PAGE, Store};
use crate::transcript::{Key, Roots};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) fn fixture(label: &str) -> (PathBuf, Roots, PathBuf) {
    let name = format!(
        "{label}-{}-{}",
        std::process::id(),
        crate::terminal::SessionId::new().0
    );
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/phase-three-conversations-and-prompts/tests")
        .join(name);
    let roots = Roots {
        codex: dir.join("codex"),
        agy: dir.join("brain"),
        kimi: dir.join("kimi"),
        grok: dir.join("grok"),
    };
    std::fs::create_dir_all(roots.codex.join("sessions")).unwrap();
    std::fs::create_dir_all(&roots.agy).unwrap();
    std::fs::create_dir_all(roots.kimi.join("sessions")).unwrap();
    std::fs::create_dir_all(roots.grok.join("sessions")).unwrap();
    let db = dir.join("index.sqlite3");
    (dir, roots, db)
}
pub(super) fn key(agent: &str, n: usize) -> Key {
    Key::new(agent, &format!("01234567-1234-4321-8765-{n:012x}")).unwrap()
}
pub(super) fn source(roots: &Roots, key: &Key) -> PathBuf {
    let p = if key.agent == "agy" {
        roots
            .agy
            .join(&key.id)
            .join(".system_generated/logs/transcript.jsonl")
    } else {
        roots
            .codex
            .join("sessions")
            .join(format!("rollout-test-{}.jsonl", key.id))
    };
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    p
}
pub(super) fn put(path: &Path, row: Value) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    serde_json::to_writer(&mut f, &row).unwrap();
    writeln!(f).unwrap();
}
pub(super) fn prompt(k: &Key, n: usize, body: &str, stamp: Option<&str>) -> Value {
    if k.agent == "agy" {
        json!({"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE","step_index":n,"created_at":stamp,
        "content":format!("<USER_REQUEST>\n{body}\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nmetadata\n</ADDITIONAL_METADATA>")})
    } else {
        json!({"type":"event_msg","timestamp":stamp,"payload":{"type":"item_completed","item":{"type":"UserMessage","id":format!("user-{n}"),"content":[{"type":"text","text":body}]}}})
    }
}
fn header(path: &Path, k: &Key) {
    if k.agent == "codex" {
        put(
            path,
            json!({"type":"session_meta","payload":{"id":k.id,"cli_version":"0.171.0"}}),
        );
    }
}
fn drain(s: &mut Store, k: &Key, dir: &Path, p: &Path) {
    while s.sync(k, dir, p).unwrap() {}
}
#[test]
fn accepted_prompts_only_preserve_duplicates_multiline_and_originals() {
    for agent in ["codex", "agy"] {
        let (dir, r, db) = fixture(agent);
        let mut s = Store::open(&db, r.clone()).unwrap();
        let k = key(agent, 1);
        let p = source(&r, &k);
        header(&p, &k);
        if agent == "codex" {
            put(
                &p,
                json!({"type":"response_item","payload":{"role":"user","content":[{"type":"input_text","text":"system context"}]}}),
            );
            put(
                &p,
                json!({"type":"event_msg","payload":{"type":"task_started"}}),
            );
        } else {
            put(
                &p,
                json!({"type":"USER_INPUT","source":"TOOL","content":"tool output"}),
            );
        }
        for (i, body) in ["相同提问", "相同提问", "第一行\n  第二行 <USER_REQUEST>"]
            .iter()
            .enumerate()
        {
            put(&p, prompt(&k, i, body, Some("2026-11-22T01:00:00Z")));
        }
        if agent == "codex" {
            put(
                &p,
                json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"final answer"}}),
            );
        } else {
            put(
                &p,
                json!({"type":"PLANNER_RESPONSE","source":"MODEL","status":"DONE","content":"final answer"}),
            );
        }
        drain(&mut s, &k, &dir, &p);
        if agent == "agy" {
            s.finish_agy(&k, 3, "NO_TOOL_CALL").unwrap();
        }
        assert_eq!(s.get(&k).unwrap().unwrap().preview, "final answer");
        assert_eq!(s.page(&k, 0).unwrap().len(), 3);
        assert_eq!(s.text(&k, 3).unwrap(), "第一行\n  第二行 <USER_REQUEST>");
        assert_eq!(s.text(&k, 1).unwrap(), s.text(&k, 2).unwrap());
        drop(s);
        let mut reopened = Store::open(&db, r).unwrap();
        drain(&mut reopened, &k, &dir, &p);
        assert_eq!(reopened.get(&k).unwrap().unwrap().count, 3);
    }
}
#[test]
fn incomplete_utf8_replay_rotation_and_unrelated_paths() {
    let (dir, r, db) = fixture("tail");
    let k = key("agy", 7);
    let p = source(&r, &k);
    let mut s = Store::open(&db, r.clone()).unwrap();
    File::create(&p).unwrap();
    assert!(!s.sync(&k, &dir, &p).unwrap());
    assert_eq!(
        s.get(&k)
            .unwrap()
            .expect("空文件也应保留已登记的位置")
            .count,
        0
    );
    assert!(s.recent().unwrap().is_empty());
    put(&p, prompt(&k, 1, "一", None));
    drain(&mut s, &k, &dir, &p);
    let mut raw = serde_json::to_string(&prompt(&k, 2, "第二行", None))
        .unwrap()
        .into_bytes();
    raw.push(b'\n');
    let cut = raw.windows(3).position(|b| b == "第".as_bytes()).unwrap() + 1;
    OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(&raw[..cut])
        .unwrap();
    assert!(!s.sync(&k, &dir, &p).unwrap());
    assert_eq!(s.page(&k, 0).unwrap().len(), 1);
    OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(&raw[cut..])
        .unwrap();
    drain(&mut s, &k, &dir, &p);
    assert_eq!(s.text(&k, 2).unwrap(), "第二行");
    let old = std::fs::read(&p).unwrap();
    std::fs::rename(&p, p.with_extension("previous")).unwrap();
    put(&p, prompt(&k, 1, "被更改的旧消息", None));
    assert!(s.sync(&k, &dir, &p).is_err());
    assert_eq!(s.page(&k, 0).unwrap().len(), 2);
    assert!(s.text(&k, 1).is_err());
    std::fs::write(&p, old).unwrap();
    put(&p, prompt(&k, 3, "三", None));
    drain(&mut s, &k, &dir, &p);
    assert_eq!(s.page(&k, 0).unwrap().len(), 3);
    assert!(s.sync(&k, &dir, &db).is_err());
    assert!(s.sync(&key("agy", 8), &dir, &p).is_err());
}
#[test]
fn recent_ten_and_concurrent_writers_keep_order() {
    let (dir, r, db) = fixture("recent");
    let mut s = Store::open(&db, r.clone()).unwrap();
    assert!(s.recent().unwrap().is_empty());
    let empty = key("codex", 10);
    let file = source(&r, &empty);
    header(&file, &empty);
    drain(&mut s, &empty, &dir, &file);
    assert!(s.recent().unwrap().is_empty());
    for n in 1..=12 {
        let k = key("agy", n);
        let p = source(&r, &k);
        put(
            &p,
            prompt(&k, 1, "topic", Some(&format!("2026-11-22T01:{n:02}:00Z"))),
        );
        drain(&mut s, &k, &dir.join(format!("project-{}", n % 3)), &p);
        assert_eq!(s.recent().unwrap().len(), n.min(10));
    }
    let expected = (3..=12).rev().map(|n| key("agy", n)).collect::<Vec<_>>();
    assert_eq!(
        s.recent()
            .unwrap()
            .into_iter()
            .map(|v| v.key)
            .collect::<Vec<_>>(),
        expected
    );
    let k = key("agy", 1);
    let p = source(&r, &k);
    let original = std::fs::read(&p).unwrap();
    std::fs::write(&p, original).unwrap();
    drain(&mut s, &k, &dir, &p);
    assert_eq!(s.recent().unwrap().first().unwrap().key, key("agy", 12));
    put(&p, prompt(&k, 2, "new", Some("2026-11-22T05:00:00+00:00")));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut other = Store::open(&db, r.clone()).unwrap();
            drain(&mut other, &k, &dir, &p);
        });
        drain(&mut s, &k, &dir, &p);
    });
    let current = s.recent().unwrap();
    assert_eq!(current.len(), 10);
    assert_eq!(current[0].key, k);
    assert_eq!(current[0].count, 2);
    assert!(!current.iter().any(|v| v.key == key("agy", 3)));
    assert_eq!(s.projects().unwrap().len(), 3);
    for (agent, n, time) in [
        ("codex", 22, None),
        ("agy", 22, None),
        ("codex", 21, Some("2026-11-22T14:00:00+08:00")),
        ("agy", 21, Some("2026-11-22T06:00:00Z")),
    ] {
        let k = key(agent, n);
        let file = source(&r, &k);
        header(&file, &k);
        put(&file, prompt(&k, 1, "tie", time));
        drain(&mut s, &k, &dir, &file);
    }
    drop(s);
    let reopened = Store::open(&db, r).unwrap();
    let ordered: Vec<_> = reopened
        .history(None, 20, 0)
        .unwrap()
        .into_iter()
        .map(|v| v.key)
        .collect();
    assert_eq!(&ordered[..2], &[key("agy", 21), key("codex", 21)]);
    assert_eq!(
        &ordered[ordered.len() - 2..],
        &[key("agy", 22), key("codex", 22)]
    );
}
#[test]
fn large_history_is_paged_and_bodies_are_loaded_by_position() {
    let (dir, r, db) = fixture("large");
    let k = key("agy", 9);
    let p = source(&r, &k);
    let mut f = std::io::BufWriter::new(File::create(&p).unwrap());
    for n in 1..=10_000 {
        let body = if n == 2 {
            format!("{}\n  远处的记号", "较长的原文".repeat(52))
        } else {
            format!("原文 {n}\n  中文")
        };
        serde_json::to_writer(&mut f, &prompt(&k, n, &body, None)).unwrap();
        writeln!(f).unwrap();
    }
    drop(f);
    let mut s = Store::open(&db, r).unwrap();
    drain(&mut s, &k, &dir, &p);
    assert_eq!(s.get(&k).unwrap().unwrap().count, 10_000);
    assert_eq!(s.page(&k, 0).unwrap().len(), PAGE);
    assert_eq!(s.page(&k, 0).unwrap()[0].number, 10_000);
    assert_eq!(s.page(&k, 9_999).unwrap()[0].number, 1);
    assert_eq!(s.text(&k, 1).unwrap(), "原文 1\n  中文");
    let mut hits = Vec::new();
    let mut before = Some(i64::MAX);
    while let Some(cursor) = before {
        before = s.search_batch(&k, "远处的记号", cursor, &mut hits).unwrap();
    }
    assert_eq!(hits.iter().map(|hit| hit.number).collect::<Vec<_>>(), [2]);
    assert!(!hits[0].snippet.contains("远处的记号"));
    assert!(
        s.text(&k, hits[0].number)
            .unwrap()
            .ends_with("\n  远处的记号")
    );
    hits.clear();
    s.search_batch(&k, "原文", i64::MAX, &mut hits).unwrap();
    assert_eq!(hits.len(), PAGE + 1);
}

#[test]
fn interrupted_writer_reopens_at_the_same_prompt_and_cursor() {
    const CHILD: &str = "MCLI_QA_INTERRUPT_INDEX";
    if let Some(path) = std::env::var_os(CHILD) {
        let dir = PathBuf::from(path);
        let roots = Roots {
            codex: dir.join("codex"),
            agy: dir.join("brain"),
            kimi: dir.join("kimi"),
            grok: dir.join("grok"),
        };
        let k = key("agy", 4);
        let mut store = Store::open(&dir.join("index.sqlite3"), roots.clone()).unwrap();
        store.sync(&k, &dir, &source(&roots, &k)).unwrap();
        panic!("测试进程未在预期事务边界退出");
    }
    for stage in ["before", "after"] {
        let (dir, roots, db) = fixture(stage);
        let k = key("agy", 4);
        let file = source(&roots, &k);
        put(&file, prompt(&k, 0, "first", None));
        let mut store = Store::open(&db, roots.clone()).unwrap();
        drain(&mut store, &k, &dir, &file);
        drop(store);
        put(&file, prompt(&k, 1, "第二条\n  原文", None));
        let status=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","tests::conversation_index::interrupted_writer_reopens_at_the_same_prompt_and_cursor"])
            .env(CHILD,&dir).env("MCLI_QA_COMMIT_BOUNDARY",stage)
            .stdout(std::process::Stdio::null()).status().unwrap();
        assert_eq!(status.code(), Some(43));
        let mut store = Store::open(&db, roots).unwrap();
        assert_eq!(
            store.get(&k).unwrap().unwrap().count,
            if stage == "before" { 1 } else { 2 }
        );
        drain(&mut store, &k, &dir, &file);
        assert_eq!(store.get(&k).unwrap().unwrap().count, 2);
        assert_eq!(store.text(&k, 2).unwrap(), "第二条\n  原文");
    }
}

#[test]
fn abbreviated_codex_display_needs_a_known_id_and_one_matching_official_header() {
    let (dir, roots, db) = fixture("display");
    let id = "01a201e9-c753-7872-ba0a-1a1bb35a0d7e";
    let prefix = &id[..29];
    let k = Key::new("codex", id).unwrap();
    let part = roots.codex.join("sessions/2026/11/22");
    std::fs::create_dir_all(&part).unwrap();
    let path = part.join(format!("rollout-sample-{id}.jsonl"));
    header(&path, &k);
    let mut store = Store::open(&db, roots).unwrap();
    assert_eq!(store.known_codex_display(prefix).unwrap(), None);
    drain(&mut store, &k, &dir, &path);
    assert_eq!(
        store.known_codex_display(prefix).unwrap().as_deref(),
        Some(id)
    );
    let other = Key::new("codex", &format!("{prefix}eeeeeee")).unwrap();
    header(
        &part.join(format!("rollout-sample-{}.jsonl", other.id)),
        &other,
    );
    assert_eq!(store.known_codex_display(prefix).unwrap(), None);
}

#[test]
fn failed_or_cancelled_turn_keeps_only_an_accepted_question() {
    let (dir, roots, db) = fixture("outcome");
    let mut store = Store::open(&db, roots.clone()).unwrap();
    let k = key("codex", 2);
    let p = source(&roots, &k);
    header(&p, &k);
    put(
        &p,
        json!({"type":"event_msg","payload":{"type":"task_started"}}),
    );
    put(
        &p,
        json!({"type":"event_msg","payload":{"type":"turn_aborted"}}),
    );
    drain(&mut store, &k, &dir, &p);
    assert!(store.recent().unwrap().is_empty());
    put(&p, prompt(&k, 1, "已经被接收", None));
    put(
        &p,
        json!({"type":"event_msg","payload":{"type":"error","message":"fixture failure"}}),
    );
    put(
        &p,
        json!({"type":"event_msg","payload":{"type":"task_complete","last_agent_message":null}}),
    );
    drain(&mut store, &k, &dir, &p);
    let summary = store.get(&k).unwrap().unwrap();
    assert_eq!(
        (
            summary.count,
            summary.phase.as_str(),
            summary.preview.as_str()
        ),
        (1, "出错", "已经被接收")
    );
}

#[test]
#[ignore = "需要隔离的官方 Codex 提交拦截样例，对照真实 rollout 和本机索引"]
fn official_codex_blocked_submission_is_absent_from_footprints() {
    let name = std::env::var("MCLI_QA_BLOCK_CASE").unwrap();
    let suffix = name.strip_prefix("denied-").unwrap();
    assert!(suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit()));
    let case = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/phase-three-conversations-and-prompts")
        .join(name);
    let evidence: Value =
        serde_json::from_slice(&std::fs::read(case.join("verified.json")).unwrap()).unwrap();
    assert_eq!(evidence["requests_after_each_submission"], json!([0, 1]));
    assert_eq!(evidence["turns"][0]["hook_statuses"], json!(["blocked"]));
    assert_eq!(evidence["turns"][1]["hook_statuses"], json!(["completed"]));
    let (dir, mut roots, db) = fixture("blocked-submission");
    roots.codex = case.join("codex");
    let k = Key::new("codex", evidence["id"].as_str().unwrap()).unwrap();
    let source = case.join(evidence["source"].as_str().unwrap());
    assert!(roots.candidate(&k, &source));
    let project = case.join("project");
    for _ in 0..2 {
        let mut store = Store::open(&db, roots.clone()).unwrap();
        drain(&mut store, &k, &project, &source);
        let recent = store.recent().unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].key, k);
        assert_eq!(recent[0].count, 1);
        assert_eq!(
            store.text(&k, 1).unwrap(),
            "Mcli isolated accepted submission"
        );
    }
    std::fs::write(
        dir.join("verified.json"),
        serde_json::to_vec(&json!({
            "thread":k.id,"indexed_prompts":1,"after_reopen":1,"blocked_prompt_present":false
        }))
        .unwrap(),
    )
    .unwrap();
}
