//! `.evren/docs_state.json` (Python `project_docs.py` `load_state`/`save_state`):
//! bölüm karmaları için küçük durum dosyası. Okuma hatalarında boş şema döner;
//! yazım `json.dumps(indent=2, ensure_ascii=False) + "\n"` ile aynıdır ve
//! `.json.tmp` üzerinden atomik yapılır.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::get_evren_dir;
use crate::error::EvrenError;

/// Durum dosyası şema sürümü.
pub const SCHEMA_VERSION: i64 = 1;

/// `<root>/.evren/docs_state.json`.
pub fn state_path(root: &Path) -> PathBuf {
    get_evren_dir(root).join("docs_state.json")
}

/// Boş durum: `{"schema_version": 1, "sections": {}}`.
pub fn empty_state() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("schema_version".into(), json!(SCHEMA_VERSION));
    m.insert("sections".into(), Value::Object(Map::new()));
    m
}

/// Dosyayı okur (UTF-8, BOM toleranslı). Hata ya da nesne olmayan içerik → boş
/// durum. `schema_version` eksikse eklenir; `sections` nesne değilse sıfırlanır.
pub fn load_state(root: &Path) -> Map<String, Value> {
    let path = state_path(root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return empty_state(),
    };
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let mut data = match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(m)) => m,
        _ => return empty_state(),
    };
    data.entry("schema_version")
        .or_insert_with(|| json!(SCHEMA_VERSION));
    let sections_ok = matches!(data.get("sections"), Some(Value::Object(_)));
    if !sections_ok {
        data.insert("sections".into(), Value::Object(Map::new()));
    }
    data
}

/// Python `json.dumps(state, ensure_ascii=False, indent=2) + "\n"`.
pub fn render_state(state: &Map<String, Value>) -> String {
    let mut s =
        serde_json::to_string_pretty(&Value::Object(state.clone())).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

/// Durumu `.json.tmp` üzerinden yazar.
pub fn save_state(root: &Path, state: &Map<String, Value>) -> Result<PathBuf, EvrenError> {
    let path = state_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EvrenError::io(parent, e))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, render_state(state)).map_err(|e| EvrenError::io(&tmp, e))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        // Windows'ta hedef varsa rename başarısız olabilir: önce sil, yeniden dene.
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&tmp, &path).map_err(|_| EvrenError::io(&path, e))?;
    }
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_broken() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_state(dir.path()), empty_state());
        std::fs::create_dir_all(get_evren_dir(dir.path())).unwrap();
        std::fs::write(state_path(dir.path()), "[1, 2]").unwrap();
        assert_eq!(load_state(dir.path()), empty_state());
        std::fs::write(state_path(dir.path()), "\u{feff}{\"sections\": 5}").unwrap();
        let st = load_state(dir.path());
        assert_eq!(st.get("schema_version"), Some(&json!(1)));
        assert_eq!(st.get("sections"), Some(&json!({})));
    }

    #[test]
    fn save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = empty_state();
        st.insert("sections".into(), json!({"a": {"hash": "ç"}}));
        save_state(dir.path(), &st).unwrap();
        let raw = std::fs::read_to_string(state_path(dir.path())).unwrap();
        assert_eq!(
            raw,
            "{\n  \"schema_version\": 1,\n  \"sections\": {\n    \"a\": {\n      \"hash\": \"ç\"\n    }\n  }\n}\n"
        );
        assert_eq!(load_state(dir.path()), st);
        assert!(!dir.path().join(".evren/docs_state.json.tmp").exists());
    }
}
