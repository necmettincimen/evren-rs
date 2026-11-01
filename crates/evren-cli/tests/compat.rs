//! `tests/compat/` Python uyumluluk testini cargo'dan çalıştırır (Python + evren-cli gerekir).
//! `cargo test -p evren-cli --test compat -- --ignored`

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "Python, pytest, jsonschema ve ../evren-cli gerekir"]
#[allow(clippy::panic)]
fn python_oracle_compat() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new("python")
        .args(["-m", "pytest", "tests/compat", "-q"])
        .current_dir(&root)
        .env("EVREN_RS_BIN", env!("CARGO_BIN_EXE_evren"))
        .status();
    match status {
        Ok(s) => assert!(s.success(), "uyumluluk testi başarısız: {s}"),
        Err(e) => panic!("pytest çalıştırılamadı: {e}"),
    }
}
