//! Faz 1c birim testleri: Python `tests/test_tools.py`, `test_file_ops.py`,
//! `test_tool_args.py`, `test_proc_utils.py`, `test_background.py`,
//! `test_command_timeout.py`, `test_tool_output_truncate.py` karşılıkları.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::args::{coerce_value, normalize_tool_arguments};
use super::background::BackgroundManager;
use super::command::{spill_long_output, tool_run_command, BIG_OUTPUT_CHARS};
use super::diff::apply_edits;
use super::file_ops::{
    backup_enabled, is_safe_path, read_file_with_meta, rollback_last_backup, safe_read_file,
    safe_write_file, set_backup_enabled, write_changed_lines,
};
use super::fs_tools::{
    tool_create_plan, tool_edit_file, tool_find_files, tool_list_directory, tool_view_file,
    tool_write_file, EditArgs,
};
use super::proc::{run_with_tree_kill, stream_with_tree_kill, StreamTag, TIMEOUT_RETURN_CODE};
use super::registry::{tool_argument_required, tool_argument_types, tools_schema};
use super::{execute_tool_call, ToolContext};
use crate::config::Config;

/// Yedek bayrağı süreç genelinde paylaşılır; ona dokunan testler sıraya girer.
static BACKUP_LOCK: Mutex<()> = Mutex::new(());

fn workspace() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn ctx_for(dir: &Path) -> ToolContext {
    ToolContext::auto(dir)
}

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, content).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

fn python_cmd(code: &str) -> Command {
    let mut c = Command::new("python");
    c.args(["-c", code]);
    c
}

fn argv(code: &str) -> Vec<String> {
    vec!["python".to_string(), "-c".to_string(), code.to_string()]
}

// ============================================================ test_tools.py

#[test]
fn view_file_range() {
    let ws = workspace();
    write(ws.path(), "example.py", "line1\nline2\nline3\nline4\n");
    let ctx = ctx_for(ws.path());
    let result = tool_view_file(&ctx, "example.py", Some(2), Some(3));
    assert!(result.contains("line2"), "{result}");
    assert!(result.contains("line3"));
    assert!(!result.contains("line1"));
}

#[test]
fn write_and_edit_file() {
    let ws = workspace();
    let ctx = ctx_for(ws.path());
    let write_res = tool_write_file(&ctx, "script.py", "def foo():\n    return 1\n", true, false);
    assert!(write_res.contains("BAŞARILI"), "{write_res}");
    let edit_res = tool_edit_file(
        &ctx,
        EditArgs {
            path: "script.py",
            target_snippet: "return 1",
            replacement_snippet: "return 100",
            auto_approve: true,
            ..EditArgs::default()
        },
    );
    assert!(edit_res.contains("BAŞARILI"), "{edit_res}");
    assert!(read(ws.path(), "script.py").contains("return 100"));
}

#[test]
fn list_and_find() {
    let ws = workspace();
    write(ws.path(), "folder/sub.txt", "hello world");
    let ctx = ctx_for(ws.path());
    let list_res = tool_list_directory(&ctx, ".", true);
    assert!(list_res.contains("folder/sub.txt"), "{list_res}");
    let find_res = tool_find_files(&ctx, "*.txt", "world", 0);
    assert!(find_res.contains("folder/sub.txt"), "{find_res}");
}

#[test]
fn find_files_lists_without_query_and_context_blocks() {
    let ws = workspace();
    write(ws.path(), "a.txt", "one\ntwo\nthree\nfour\nfive\n");
    write(ws.path(), "b.md", "nothing");
    let ctx = ctx_for(ws.path());
    let listed = tool_find_files(&ctx, "*.txt", "", 0);
    assert_eq!(listed, "a.txt");
    let ctxd = tool_find_files(&ctx, "*.txt", "three", 1);
    assert_eq!(ctxd, "a.txt-2- two\na.txt:3: three\na.txt-4- four");
    assert_eq!(
        tool_find_files(&ctx, "*.txt", "yok", 0),
        "Eşleşme bulunamadı."
    );
    assert_eq!(
        tool_find_files(&ctx, "*.rs", "", 0),
        "Eşleşen dosya bulunamadı."
    );
}

#[test]
fn execute_tool_dispatch_missing_file() {
    let ws = workspace();
    let ctx = ctx_for(ws.path());
    let res = execute_tool_call(&ctx, "view_file", json!({"path": "non_existent.py"}), false);
    assert!(res.starts_with("HATA:"), "{res}");
}

#[test]
fn create_plan_ok_and_invalid() {
    let ws = workspace();
    let ctx = ctx_for(ws.path());
    let res = tool_create_plan(&ctx, "my-plan", "# Plan\n\nAdım 1", true);
    assert!(res.contains("BAŞARILI"), "{res}");
    let plan_file = ws.path().join("plans").join("my-plan").join("plan.md");
    assert!(plan_file.exists());
    assert!(std::fs::read_to_string(plan_file)
        .unwrap()
        .contains("Adım 1"));
    let bad = tool_create_plan(&ctx, "!!!", "content", true);
    assert!(bad.starts_with("HATA:"));
    let via = execute_tool_call(
        &ctx,
        "create_plan",
        json!({"name": "dispatch-plan", "content": "# x"}),
        true,
    );
    assert!(via.contains("BAŞARILI"), "{via}");
}

#[test]
fn web_fetch_schema_and_invalid_scheme() {
    let entry = tools_schema()
        .iter()
        .find(|t| t["function"]["name"] == "web_fetch")
        .expect("web_fetch şeması");
    let params = &entry["function"]["parameters"];
    assert!(params["properties"].get("url").is_some());
    assert_eq!(params["required"], json!(["url"]));
    let ws = workspace();
    let ctx = ctx_for(ws.path());
    let res = execute_tool_call(
        &ctx,
        "web_fetch",
        json!({"url": "ftp://example.com/x"}),
        false,
    );
    assert!(res.starts_with("HATA:"), "{res}");
}

#[test]
fn edit_file_large_file_keeps_tail_and_size() {
    let big: String = (0..12000).map(|i| format!("LINE_{i:05} = {i}\n")).collect();
    assert!(big.len() > 150_000);
    let ws = workspace();
    let target = ws.path().join("big.py");
    std::fs::write(&target, big.as_bytes()).unwrap();
    let before = std::fs::metadata(&target).unwrap().len();
    let ctx = ctx_for(ws.path());
    let res = tool_edit_file(
        &ctx,
        EditArgs {
            path: "big.py",
            target_snippet: "LINE_00000 = 0\n",
            replacement_snippet: "LINE_00000 = 0\nEVREN_SHELL_LABEL = 'x'\n",
            auto_approve: true,
            ..EditArgs::default()
        },
    );
    assert!(res.contains("BAŞARILI"), "{res}");
    let content = std::fs::read_to_string(&target).unwrap();
    assert!(content.contains("EVREN_SHELL_LABEL = 'x'"));
    assert!(content.contains("LINE_11999 = 11999"));
    assert!(!content.contains("karakterde kesildi"));
    assert_eq!(
        std::fs::metadata(&target).unwrap().len(),
        before + "EVREN_SHELL_LABEL = 'x'\n".len() as u64
    );
}

#[test]
fn edit_file_reports_diff_summary() {
    let ws = workspace();
    write(ws.path(), "summary.py", "a = 1\nb = 2\nc = 3\n");
    let ctx = ctx_for(ws.path());
    let res = tool_edit_file(
        &ctx,
        EditArgs {
            path: "summary.py",
            target_snippet: "b = 2\n",
            replacement_snippet: "b = 20\nd = 4\n",
            auto_approve: true,
            ..EditArgs::default()
        },
    );
    assert!(res.contains("BAŞARILI"), "{res}");
    assert!(res.contains("Diff özeti:"));
    assert!(res.contains("+2/-1 satır"), "{res}");
    assert!(res.to_lowercase().contains("doğrulama gerekmez"));
    assert!(res.contains("geri kalanı değişmedi"));
    assert_eq!(
        read(ws.path(), "summary.py"),
        "a = 1\nb = 20\nd = 4\nc = 3\n"
    );
}

#[test]
fn edit_file_writes_only_changed_span() {
    let ws = workspace();
    let old = "a\nb\nc\nd\n";
    write(ws.path(), "span.txt", old);
    write_changed_lines("span.txt", old, "a\nB\nc\nd\n", ws.path(), None).unwrap();
    assert_eq!(read(ws.path(), "span.txt"), "a\nB\nc\nd\n");
    write_changed_lines("span.txt", "a\nB\nc\nd\n", "a\nc\nd\n", ws.path(), None).unwrap();
    assert_eq!(read(ws.path(), "span.txt"), "a\nc\nd\n");
}

#[test]
fn edit_file_range_and_guards() {
    let ws = workspace();
    write(ws.path(), "r.txt", "one\ntwo\nthree\n");
    let ctx = ctx_for(ws.path());
    let start = json!(2);
    let end = json!(2);
    let res = tool_edit_file(
        &ctx,
        EditArgs {
            path: "r.txt",
            replacement_snippet: "TWO",
            start_line: Some(&start),
            end_line: Some(&end),
            auto_approve: true,
            ..EditArgs::default()
        },
    );
    assert!(res.contains("BAŞARILI"), "{res}");
    assert_eq!(read(ws.path(), "r.txt"), "one\nTWO\nthree\n");
    let missing = tool_edit_file(
        &ctx,
        EditArgs {
            path: "yok.txt",
            target_snippet: "a",
            replacement_snippet: "b",
            auto_approve: true,
            ..EditArgs::default()
        },
    );
    assert!(missing.contains("write_file kullan"), "{missing}");
    let probe = tool_edit_file(
        &ctx,
        EditArgs {
            path: "r.txt",
            target_snippet: "TODO",
            replacement_snippet: "b",
            auto_approve: true,
            ..EditArgs::default()
        },
    );
    assert!(probe.contains("yer tutucu"), "{probe}");
}

#[test]
fn approval_refusal_blocks_edit() {
    let ws = workspace();
    write(ws.path(), "x.txt", "a\n");
    let mut env = BTreeMap::new();
    env.insert("EVREN_AUTO_APPROVE".to_string(), "false".to_string());
    env.insert("EVREN_SHELL".to_string(), "cmd".to_string());
    let ctx = ToolContext::new(ws.path(), &Config::from_env(env))
        .with_approver(Box::new(|_| false.into()));
    let res = execute_tool_call(
        &ctx,
        "edit_file",
        json!({"path": "x.txt", "target_snippet": "a", "replacement_snippet": "b"}),
        false,
    );
    assert!(res.starts_with("İPTAL"), "{res}");
    assert_eq!(read(ws.path(), "x.txt"), "a\n");
}

#[test]
fn apply_patch_and_clone_tree_dispatch() {
    let ws = workspace();
    write(ws.path(), "p.txt", "a\nb\nc\n");
    let ctx = ctx_for(ws.path());
    let patch = "--- a/p.txt\n+++ b/p.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
    let res = execute_tool_call(&ctx, "apply_patch", json!({"patch": patch}), true);
    assert_eq!(res, "1 dosya yamalandı: p.txt");
    assert_eq!(read(ws.path(), "p.txt"), "a\nB\nc\n");
    assert!(
        execute_tool_call(&ctx, "apply_patch", json!({"patch": "bos"}), true).starts_with("HATA:")
    );

    write(ws.path(), "src/Foo/Foo.cs", "class Foo {}");
    let res = execute_tool_call(
        &ctx,
        "clone_tree",
        json!({"src": "src/Foo", "dst": "src/Bar", "replacements": [["Foo", "Bar"]]}),
        true,
    );
    assert!(
        res.starts_with("BAŞARILI: 1 dosya (1 içerik dönüştü, 0 atlandı/mevcut)"),
        "{res}"
    );
    assert_eq!(read(ws.path(), "src/Bar/Bar.cs"), "class Bar {}");
}

// ========================================================= test_file_ops.py

fn backup_count(ws: &Path) -> usize {
    let dir = ws.join(".evren").join("backups");
    if !dir.exists() {
        return 0;
    }
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "bak"))
        .count()
}

#[test]
fn safe_path_boundary_and_forbidden() {
    let ws = workspace();
    let (safe, _) = is_safe_path(&ws.path().join("src").join("main.py"), ws.path(), false);
    assert!(safe);
    let (safe_out, reason) = is_safe_path(
        &ws.path().parent().unwrap().join("secret.txt"),
        ws.path(),
        false,
    );
    assert!(!safe_out);
    assert!(reason.contains("çalışma dizini"), "{reason}");
    let (git_ok, reason) = is_safe_path(&ws.path().join(".git").join("config"), ws.path(), false);
    assert!(!git_ok);
    assert!(reason.contains("engellendi"), "{reason}");
}

#[test]
fn safe_write_and_read() {
    let ws = workspace();
    let backup = safe_write_file("test.txt", "Merhaba Dunya", ws.path(), None, None).unwrap();
    assert!(backup.is_none());
    assert_eq!(
        safe_read_file("test.txt", ws.path(), 0).unwrap(),
        "Merhaba Dunya"
    );
}

#[test]
fn backup_and_rollback() {
    let ws = workspace();
    safe_write_file("test.txt", "Version 1", ws.path(), None, None).unwrap();
    let backup = safe_write_file("test.txt", "Version 2", ws.path(), Some(true), None).unwrap();
    let backup = backup.expect("yedek yolu");
    assert!(backup.exists());
    assert_eq!(
        safe_read_file("test.txt", ws.path(), 0).unwrap(),
        "Version 2"
    );
    let (ok, msg) = rollback_last_backup(&ws.path().join("test.txt"), ws.path());
    assert!(ok, "{msg}");
    assert_eq!(
        safe_read_file("test.txt", ws.path(), 0).unwrap(),
        "Version 1"
    );
}

#[test]
fn backup_flag_behaviour() {
    let _guard = BACKUP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_backup_enabled(false);
    assert!(!backup_enabled());
    let ws = workspace();
    safe_write_file("test.txt", "Version 1", ws.path(), None, None).unwrap();
    let backup = safe_write_file("test.txt", "Version 2", ws.path(), None, None).unwrap();
    assert!(backup.is_none());
    assert_eq!(backup_count(ws.path()), 0);
    assert_eq!(
        safe_read_file("test.txt", ws.path(), 0).unwrap(),
        "Version 2"
    );

    set_backup_enabled(true);
    assert!(backup_enabled());
    let ws2 = workspace();
    safe_write_file("test.txt", "Version 1", ws2.path(), None, None).unwrap();
    let backup = safe_write_file("test.txt", "Version 2", ws2.path(), None, None).unwrap();
    assert!(backup.is_some());
    assert_eq!(backup_count(ws2.path()), 1);
    set_backup_enabled(false);
}

#[test]
fn read_meta_crlf_and_bom() {
    let ws = workspace();
    std::fs::write(ws.path().join("crlf.txt"), b"line1\r\nline2\r\n").unwrap();
    let (_, meta) = read_file_with_meta("crlf.txt", ws.path(), 0).unwrap();
    assert_eq!(meta.newline(), "\r\n");
    assert!(!meta.has_bom);
    std::fs::write(ws.path().join("bom.txt"), b"\xef\xbb\xbfhello").unwrap();
    let (content, meta) = read_file_with_meta("bom.txt", ws.path(), 0).unwrap();
    assert!(meta.has_bom);
    assert_eq!(content, "hello");
}

#[test]
fn write_preserves_crlf_and_bom() {
    let ws = workspace();
    std::fs::write(ws.path().join("crlf.txt"), b"a\r\nb\r\n").unwrap();
    let (_, meta) = read_file_with_meta("crlf.txt", ws.path(), 0).unwrap();
    safe_write_file("crlf.txt", "a\r\nc\r\n", ws.path(), None, Some(&meta)).unwrap();
    let raw = std::fs::read(ws.path().join("crlf.txt")).unwrap();
    assert!(raw.windows(2).any(|w| w == b"\r\n"));
    assert!(!raw.windows(3).any(|w| w == b"\r\r\n"));

    std::fs::write(ws.path().join("bom.txt"), b"\xef\xbb\xbfhello").unwrap();
    let (_, meta) = read_file_with_meta("bom.txt", ws.path(), 0).unwrap();
    safe_write_file("bom.txt", "world", ws.path(), None, Some(&meta)).unwrap();
    let raw = std::fs::read(ws.path().join("bom.txt")).unwrap();
    assert!(raw.starts_with(b"\xef\xbb\xbf"));
}

// ======================================================== test_tool_args.py

#[test]
fn coerce_scalar_and_container() {
    let cases: Vec<(&str, &str, &str, Value)> = vec![
        (r#"[{"a": 1}]"#, "array", "edits", json!([{"a": 1}])),
        ("12", "integer", "start_line", json!(12)),
        ("true", "boolean", "all", json!(true)),
        ("False", "boolean", "all", json!(false)),
        ("1", "boolean", "all", json!(true)),
    ];
    for (value, ty, field, expected) in cases {
        assert_eq!(
            coerce_value(json!(value), ty, field).unwrap(),
            expected,
            "{value} as {ty}"
        );
    }
    let err = coerce_value(json!("def foo(): pass"), "array", "edits").unwrap_err();
    assert!(err.to_string().contains("JSON"), "{err}");
}

#[test]
fn normalize_edits_and_required() {
    let types = Some(tool_argument_types());
    let required = Some(tool_argument_required());
    let args = normalize_tool_arguments(
        "edit_file",
        json!({"path": "a.py", "edits": r#"[{"target_snippet": "x", "replacement_snippet": "y", "all": "true"}]"#}),
        types,
        required,
    )
    .unwrap();
    let edits = args["edits"].as_array().unwrap();
    assert!(edits[0].is_object());
    assert_eq!(edits[0]["all"], json!(true));

    let args = normalize_tool_arguments(
        "edit_file",
        json!({"path": "a.py", "edits": r#"[{"old_text": "x", "new_text": "y"}]"#}),
        types,
        required,
    )
    .unwrap();
    let first = &args["edits"][0];
    assert_eq!(first["target_snippet"], "x");
    assert_eq!(first["replacement_snippet"], "y");
    assert!(first.get("old_text").is_none());
    assert!(first.get("new_text").is_none());

    let args = normalize_tool_arguments(
        "edit_file",
        json!({"path": "a.py", "target_snippet": ""}),
        types,
        required,
    )
    .unwrap();
    assert_eq!(args["target_snippet"], "");

    let err = normalize_tool_arguments("view_file", json!({"start_line": 1}), types, required)
        .unwrap_err();
    assert!(err.to_string().contains("path"), "{err}");
}

#[test]
fn apply_edits_string_item_error() {
    let err = apply_edits("hello", &[json!("target")]).unwrap_err();
    assert!(err.contains("bir nesne olmalı"), "{err}");
}

#[test]
fn execute_normalization_edit_and_view() {
    let ws = workspace();
    write(ws.path(), "page.xsl", "<root/>\n");
    let ctx = ctx_for(ws.path());
    let res = execute_tool_call(
        &ctx,
        "edit_file",
        json!({"path": "page.xsl", "edits": r#"[{"old_text": "<root/>", "new_text": "<html><body/></html>"}]"#}),
        true,
    );
    assert!(res.contains("BAŞARILI"), "{res}");
    assert!(read(ws.path(), "page.xsl").contains("<html>"));

    write(ws.path(), "n.txt", "1\n2\n3\n");
    let res = execute_tool_call(
        &ctx,
        "view_file",
        json!({"path": "n.txt", "start_line": "2", "end_line": "3"}),
        false,
    );
    assert!(res.contains("2 | 2"), "{res}");
    assert!(res.contains("3 | 3"));
}

// ======================================================= test_proc_utils.py

#[test]
fn timeout_kills_grandchild() {
    let script = "import subprocess, sys, time;\
subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)']);\
time.sleep(60)";
    let start = Instant::now();
    let out = run_with_tree_kill(&mut python_cmd(script), Some(Duration::from_secs(1))).unwrap();
    assert!(out.timed_out);
    assert!(start.elapsed() < Duration::from_secs(20));
}

#[test]
fn streams_stdout_and_stderr_live() {
    let script = "import sys\nprint('out1', flush=True)\nprint('err1', file=sys.stderr, flush=True)\nprint('out2', flush=True)\n";
    let lines = Arc::new(Mutex::new(Vec::<(StreamTag, String)>::new()));
    let sink = Arc::clone(&lines);
    let out = stream_with_tree_kill(
        &mut python_cmd(script),
        Some(Duration::from_secs(30)),
        move |tag, line| {
            sink.lock()
                .unwrap()
                .push((tag, line.trim_end().to_string()));
        },
    )
    .unwrap();
    assert!(!out.timed_out);
    assert_eq!(out.code, 0);
    let lines = lines.lock().unwrap();
    assert!(
        lines.contains(&(StreamTag::Stdout, "out1".to_string())),
        "{lines:?}"
    );
    assert!(lines.contains(&(StreamTag::Stderr, "err1".to_string())));
    assert!(lines.contains(&(StreamTag::Stdout, "out2".to_string())));
    assert!(out.stdout.contains("out1") && out.stdout.contains("out2"));
    assert!(out.stderr.contains("err1"));
}

// ======================================================= test_background.py

fn wait_done(mgr: &BackgroundManager, id: &str) -> (String, String, i32) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut output = String::new();
    let mut status = String::new();
    while Instant::now() < deadline {
        let (chunk, st, code) = mgr.check(id).expect("kayıt");
        output.push_str(&chunk);
        status = st;
        if let Some(c) = code {
            return (output, status, c);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{id} 20 sn içinde bitmedi: {status}");
}

#[test]
fn background_start_and_check() {
    let ws = workspace();
    let mut mgr = BackgroundManager::new();
    let id = mgr
        .start(
            "print",
            ws.path(),
            Some(&argv("print('satir-1'); print('satir-2')")),
            &[],
        )
        .unwrap()
        .id
        .clone();
    assert_eq!(id, "bg-1");
    let (output, status, code) = wait_done(&mgr, &id);
    assert_eq!(code, 0);
    assert!(
        output.contains("satir-1") && output.contains("satir-2"),
        "{output}"
    );
    assert!(
        status.contains("bitti") && status.contains("çıkış kodu: 0"),
        "{status}"
    );
}

#[test]
fn background_stop_kills_long_running() {
    let ws = workspace();
    let mut mgr = BackgroundManager::new();
    let id = mgr
        .start(
            "long",
            ws.path(),
            Some(&argv("import time; time.sleep(600)")),
            &[],
        )
        .unwrap()
        .id
        .clone();
    assert!(mgr.get(&id).unwrap().running());
    assert!(mgr.stop(&id));
    let rec = mgr.get(&id).unwrap();
    assert!(!rec.running());
    assert!(rec.stopped);
    assert!(rec.status_line().contains("durduruldu"));
}

#[test]
fn background_cleanup_all() {
    let ws = workspace();
    let mut mgr = BackgroundManager::new();
    let sleep = argv("import time; time.sleep(600)");
    let r1 = mgr
        .start("s1", ws.path(), Some(&sleep), &[])
        .unwrap()
        .id
        .clone();
    let r2 = mgr
        .start("s2", ws.path(), Some(&sleep), &[])
        .unwrap()
        .id
        .clone();
    let r3 = mgr
        .start("done", ws.path(), Some(&argv("print('bitti')")), &[])
        .unwrap()
        .id
        .clone();
    wait_done(&mgr, &r3);
    assert_eq!(mgr.cleanup_all(), 2);
    assert!(!mgr.get(&r1).unwrap().running());
    assert!(!mgr.get(&r2).unwrap().running());
}

fn bg_id(started: &str) -> String {
    started
        .split("id: ")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn background_keeps_path_and_requires_approval() {
    let ws = workspace();
    let mut env = BTreeMap::new();
    env.insert("EVREN_AUTO_APPROVE".to_string(), "false".to_string());
    env.insert("EVREN_SHELL".to_string(), "cmd".to_string());
    let deny = ToolContext::new(ws.path(), &Config::from_env(env))
        .with_approver(Box::new(|_| false.into()));
    let refused = execute_tool_call(
        &deny,
        "run_command",
        json!({"command": "echo x", "run_in_background": true}),
        false,
    );
    assert!(refused.starts_with("İPTAL"), "{refused}");

    let ctx = ctx_for(ws.path());
    let code = "import os; print('PATH' in os.environ)";
    let started = execute_tool_call(
        &ctx,
        "run_command",
        json!({"command": code, "use_python": true, "run_in_background": true}),
        true,
    );
    let id = bg_id(&started);
    let (out, _, code) = {
        let mgr = ctx.background.lock().unwrap();
        wait_done(&mgr, &id)
    };
    assert_eq!(code, 0);
    assert!(out.contains("True"), "{out}");
    ctx.background.lock().unwrap().cleanup_all();
}

#[test]
fn check_waits_for_completion_and_returns_early() {
    let ws = workspace();
    let ctx = ctx_for(ws.path());
    let started = execute_tool_call(
        &ctx,
        "run_command",
        json!({"command": "import time; time.sleep(1); print('bitti-ok')", "use_python": true, "run_in_background": true}),
        true,
    );
    let id = bg_id(&started);
    let t0 = Instant::now();
    let out = execute_tool_call(&ctx, "check_background", json!({"id": id}), false);
    assert!(
        out.contains("bitti-ok") && out.contains("Çıkış Kodu: 0"),
        "{out}"
    );
    assert!(t0.elapsed() < Duration::from_secs(10));
    let quick = execute_tool_call(
        &ctx,
        "check_background",
        json!({"id": id, "wait_seconds": 0}),
        false,
    );
    assert!(quick.contains("yeni çıktı yok"), "{quick}");
    ctx.background.lock().unwrap().cleanup_all();
}

// ================================================== test_command_timeout.py

#[test]
fn stream_sleep_is_cut_at_timeout() {
    let start = Instant::now();
    let out = stream_with_tree_kill(
        &mut python_cmd("import time; time.sleep(400)"),
        Some(Duration::from_secs(2)),
        |_, _| {},
    )
    .unwrap();
    assert!(out.timed_out);
    assert_eq!(out.code, TIMEOUT_RETURN_CODE);
    assert!(start.elapsed() < Duration::from_secs(20));
}

#[test]
fn tool_run_command_reports_timeout_124() {
    let ws = workspace();
    let ctx = ctx_for(ws.path());
    let start = Instant::now();
    let result = tool_run_command(
        &ctx,
        "python -c \"import time; time.sleep(400)\"",
        true,
        Some(2),
        false,
        false,
    );
    assert!(start.elapsed() < Duration::from_secs(20));
    assert!(result.contains("zaman aşımı"), "{result}");
    assert!(result.starts_with("Çıkış Kodu: 124"), "{result}");
    assert!(result.contains("Komut 2 sn sonra zaman aşımına uğradı (süreç ağacı öldürüldü)"));
    assert!(!result.contains("Çıkış Kodu: -1"));
}

#[test]
fn tool_run_command_echo() {
    let ws = workspace();
    let ctx = ctx_for(ws.path());
    let result = tool_run_command(&ctx, "echo merhaba", true, None, false, false);
    assert!(result.starts_with("Çıkış Kodu: 0"), "{result}");
    assert!(result.contains("merhaba"));
}

// ============================================= test_tool_output_truncate.py

#[test]
fn huge_blob_is_not_embedded() {
    let text = "x".repeat(200_000);
    let ws = workspace();
    let result = spill_long_output(&text, Some(ws.path()), "cmd", "");
    let saved: Vec<_> = std::fs::read_dir(ws.path().join(".evren").join("tmp"))
        .unwrap()
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("cmd-") && n.ends_with(".txt")
        })
        .collect();
    assert_eq!(saved.len(), 1);
    assert_eq!(std::fs::read_to_string(saved[0].path()).unwrap(), text);
    assert!(!result.contains(&"x".repeat(100)));
    assert!(result.len() < 1000);
    assert!(result.contains("200000 karakter, 1 satır"), "{result}");
    assert!(result.contains("tek karakterin tekrarı ('x')"));
}

#[test]
fn medium_output_keeps_head_and_tail() {
    let text = (0..2000)
        .map(|i| format!("satır {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.chars().count() > 8000);
    assert!(text.chars().count() < BIG_OUTPUT_CHARS);
    let ws = workspace();
    let result = spill_long_output(&text, Some(ws.path()), "cmd", "");
    assert!(result.contains("satır 0"));
    assert!(result.contains("satır 1999"));
    assert!(result.contains("karakter kırpıldı"));
    assert!(result.contains("[EVREN] Tam çıktı (2000 satır)"));
}

#[test]
fn short_output_untouched() {
    assert_eq!(
        spill_long_output("kısa çıktı", None, "cmd", ""),
        "kısa çıktı"
    );
}

#[test]
fn memory_save_appends_project_note() {
    let ws = workspace();
    let ctx = ctx_for(ws.path());
    let empty = execute_tool_call(&ctx, "memory_save", json!({"text": "  "}), false);
    assert_eq!(empty, "HATA: memory_save için 'text' gerekli.");
    let res = execute_tool_call(
        &ctx,
        "memory_save",
        json!({"text": "testler cargo ile"}),
        false,
    );
    assert_eq!(res, "BAŞARILI: not hafızaya kaydedildi.");
    let again = execute_tool_call(
        &ctx,
        "memory_save",
        json!({"text": "testler cargo ile"}),
        false,
    );
    assert!(again.starts_with("BAŞARILI"), "{again}");
    let state = crate::memory::load_memory(ws.path(), crate::memory::Scope::Project);
    let notes = crate::memory::notes(&state);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["text"], "testler cargo ile");
}
