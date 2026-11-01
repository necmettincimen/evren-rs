//! Python'un ürettiği `ssh_hosts.json` ve `docs_state.json` dosyalarının Rust
//! tarafından aynı baytlarla okunup yazıldığını doğrular.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use evren_core::config::{get_evren_dir, get_tmp_dir};
use evren_core::docs_state::{load_state, render_state, save_state, state_path};
use evren_core::ssh_hosts::{read_registry, render_hosts, HostMapExt, SshHost};
use serde_json::{json, Value};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn ssh_hosts_python_file_roundtrips() {
    let src = fixture("ssh_hosts_python.json");
    // Python `write_text` Windows'ta CRLF yazar; Rust her yerde LF yazar.
    let raw = std::fs::read_to_string(&src).unwrap().replace("\r\n", "\n");
    let hosts = read_registry(&src).unwrap();
    assert_eq!(hosts.aliases(), vec!["koyu3", "lab"]);
    let k = hosts.get_alias("koyu3").unwrap();
    assert_eq!(k.target(), "root@10.0.0.3");
    assert_eq!(k.port, 22);
    assert_eq!(k.identity_file, "~/.ssh/id_ed25519");
    assert_eq!(k.description, "üretim sunucusu");
    assert!(k.allow_write);
    let lab = hosts.get_alias("lab").unwrap();
    assert_eq!(lab.target(), "lab.example.com");
    assert!(!lab.allow_write);

    // Python `json.dumps(indent=2, ensure_ascii=False)` ile bayt bayt aynı.
    assert_eq!(render_hosts(&hosts), raw);

    // Rust'ın yazdığı dosyayı yeniden okumak aynı kayıtları verir.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ssh_hosts.json");
    std::fs::write(&p, render_hosts(&hosts)).unwrap();
    assert_eq!(read_registry(&p).unwrap(), hosts);

    // serde ile doğrudan da okunabilir (alias dışarıdan verilir).
    let v: Value = serde_json::from_str(&raw).unwrap();
    let mut via_serde: SshHost = serde_json::from_value(v["koyu3"].clone()).unwrap();
    via_serde.alias = "koyu3".into();
    assert_eq!(&via_serde, k);
}

#[test]
fn ssh_hosts_bad_inputs() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_registry(&dir.path().join("yok.json"))
        .unwrap()
        .is_empty());
    let p = dir.path().join("ssh_hosts.json");
    std::fs::write(&p, "[1]").unwrap();
    let err = read_registry(&p).unwrap_err().to_string();
    assert!(err.starts_with("SSH host dosyası bir nesne ({alias: {...}}) olmalı: "));
    std::fs::write(&p, "{").unwrap();
    let err = read_registry(&p).unwrap_err().to_string();
    assert!(err.starts_with("SSH host dosyası geçersiz JSON içeriyor ("));
    std::fs::write(&p, r#"{"a": {"host": ""}, "b": 3, "c": {"host": "h"}}"#).unwrap();
    assert_eq!(read_registry(&p).unwrap().aliases(), vec!["c"]);
}

#[test]
fn docs_state_python_file_roundtrips() {
    let src = fixture("docs_state_python.json");
    let raw = std::fs::read_to_string(&src).unwrap().replace("\r\n", "\n");
    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(get_evren_dir(ws.path())).unwrap();
    std::fs::copy(&src, state_path(ws.path())).unwrap();

    let state = load_state(ws.path());
    assert_eq!(state.get("schema_version"), Some(&json!(1)));
    assert_eq!(state["sections"]["architecture"]["note"], "çş");
    assert_eq!(state["extra"], json!([1, 2]));
    assert_eq!(render_state(&state), raw);

    save_state(ws.path(), &state).unwrap();
    assert_eq!(std::fs::read_to_string(state_path(ws.path())).unwrap(), raw);
    assert_eq!(load_state(ws.path()), state);
}

#[test]
fn tmp_dir_layout() {
    let ws = Path::new("/ws");
    assert_eq!(get_tmp_dir(ws), ws.join(".evren").join("tmp"));
}
