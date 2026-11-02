//! `evren-cli/tests/test_repl_startup.py` + `scenarios/repl_startup.json`:
//! argümansız `evren` borulu stdin ile açılır, birkaç slash komutunu yanıtlar
//! ve `/exit` ile temiz kapanır; sohbet isteği gitmez.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Gelen isteklerin yolunu kaydeden, her isteğe 404 dönen yerel sunucu.
fn recording_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            if let Ok(mut l) = log.lock() {
                l.push(first.trim().to_string());
            }
            let mut w = &stream;
            let _ = w.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    (url, seen)
}

pub fn run_repl(inputs: &str, extra_env: &[(&str, &str)]) -> (i32, String, Vec<String>) {
    let ws = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (url, seen) = recording_server();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_evren"));
    for (k, _) in std::env::vars() {
        if k.to_uppercase().starts_with("EVREN_") {
            cmd.env_remove(k);
        }
    }
    cmd.current_dir(ws.path())
        .env("EVREN_API_KEY", "fake-key")
        .env("EVREN_BASE_URL", &url)
        .env("EVREN_DEFAULT_MODEL", "fake-model")
        .env("EVREN_CLI_HOME", home.path())
        .env("EVREN_AUTO_DOCS", "false")
        .env("EVREN_AUTO_APPROVE", "false")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(inputs.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let requests = seen.lock().map(|l| l.clone()).unwrap_or_default();
    (out.status.code().unwrap_or(-1), stdout, requests)
}

#[test]
fn repl_starts_runs_slash_commands_and_exits() {
    let (code, out, requests) = run_repl("/help\n/todos\n/bg\n/skills\n/exit\n", &[]);
    assert_eq!(code, 0, "{out}");
    for needle in [
        "EVREN CLI",
        "Bağlam:",
        "Görev listesi boş.",
        "sonlandırılıyor",
        "Sürüm Notları:",
    ] {
        assert!(out.contains(needle), "{needle} yok:\n{out}");
    }
    assert!(
        !requests.iter().any(|r| r.contains("/chat/completions")),
        "{requests:?}"
    );
}
