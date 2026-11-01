//! Python'un ürettiği oturum ve geçmiş dosyalarının Rust tarafından aynı
//! biçimde okunup yazıldığını doğrular (`tests/fixtures/*_python.*`).

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use evren_agent::history::{load_history_strings, store_string};
use evren_agent::sessions::{dumps_py, list_sessions, session_summary, sessions_dir};
use evren_agent::AgentSession;
use evren_api::ApiClient;
use evren_core::config::Config;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn test_session(ws: &Path) -> AgentSession {
    let mut env = BTreeMap::new();
    env.insert("EVREN_API_KEY".to_string(), "sk-test-dummy".to_string());
    env.insert(
        "EVREN_DEFAULT_MODEL".to_string(),
        "qwen/qwen3-coder".to_string(),
    );
    let config = Config::from_env(env);
    let client = Arc::new(ApiClient::new(&config).unwrap());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    AgentSession::new(client, config, ws, tx, CancellationToken::new())
}

#[test]
fn python_session_file_roundtrips_byte_for_byte() {
    let raw = std::fs::read_to_string(fixture("session_python.json")).unwrap();
    let value: Value = serde_json::from_str(&raw).unwrap();
    // Python `json.dumps(ensure_ascii=False)` ayraçları ile aynı bayt dizisi.
    assert_eq!(dumps_py(&value), raw);
    assert_eq!(value["title"], "README'yi güncelle");
    assert_eq!(value["messages"].as_array().unwrap().len(), 4);
}

#[test]
fn load_python_session_then_save_same_shape() {
    let ws = tempfile::tempdir().unwrap();
    let sd = sessions_dir(ws.path());
    std::fs::create_dir_all(&sd).unwrap();
    let src = fixture("session_python.json");
    let dst = sd.join("20260101-120000.json");
    std::fs::copy(&src, &dst).unwrap();

    let listed = list_sessions(ws.path(), 10);
    assert_eq!(listed, vec![dst.clone()]);
    let summary = session_summary(&dst);
    assert_eq!(
        summary,
        json!({
            "session_id": "20260101-120000",
            "title": "README'yi güncelle",
            "model": "qwen/qwen3-coder",
            "mode": "normal",
        })
    );

    let mut session = test_session(ws.path());
    assert!(session.load_session(&dst));
    assert_eq!(session.task_goal(), "README'yi güncelle");
    let msgs = session.messages();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs.len(), 5);
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "view_file");
    assert_eq!(msgs[3]["content"], "Satırlar 1-2 / 2\n# Başlık\nçğış");

    // Aynı damgaya geri yazılır; içerik Python çıktısıyla birebir aynı olmalı.
    let saved = session.save_session().unwrap();
    assert_eq!(saved, dst);
    let back = std::fs::read_to_string(&saved).unwrap();
    let orig = std::fs::read_to_string(&src).unwrap();
    assert_eq!(back, orig);

    // Bozuk dosya yüklenmez, durum değişmez.
    let bad = sd.join("20260101-130000.json");
    std::fs::write(&bad, "{\"messages\": 5}").unwrap();
    assert!(!session.load_session(&bad));
    assert_eq!(session.messages().len(), 5);
}

#[test]
fn fresh_session_without_messages_saves_nothing() {
    let ws = tempfile::tempdir().unwrap();
    let mut session = test_session(ws.path());
    assert!(session.save_session().is_none());
    assert!(!sessions_dir(ws.path()).exists());
}

#[test]
fn python_history_file_loads_newest_first() {
    let got = load_history_strings(&fixture("history_python.txt"));
    assert_eq!(
        got,
        vec!["çok\nsatırlı ğ".to_string(), "ilk komut".to_string()]
    );
}

#[test]
fn history_written_here_matches_python_layout() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("history.txt");
    store_string(&p, "ilk komut").unwrap();
    store_string(&p, "çok\nsatırlı ğ").unwrap();
    let ours = std::fs::read_to_string(&p).unwrap();
    let theirs = std::fs::read_to_string(fixture("history_python.txt")).unwrap();
    // Zaman damgaları farklıdır; başlık satırlarını normalize edip karşılaştır.
    let norm = |s: &str| -> Vec<String> {
        s.lines()
            .map(|l| {
                if l.starts_with("# ") {
                    "# <ts>".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect()
    };
    assert_eq!(norm(&ours), norm(&theirs));
    assert_eq!(
        load_history_strings(&p),
        load_history_strings(&fixture("history_python.txt"))
    );
}
