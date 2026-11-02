//! Kalıcı proje hafızası `.evren/memory.json` (Python `memory.py`).
//!
//! Proje profili (yığın, kurallar, hedef) ve serbest notlar. Genel kapsam
//! `~/.evren/memory.json` tüm projelerde geçerlidir.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::get_evren_dir;

pub const SCHEMA_VERSION: i64 = 1;

/// Ajanın ayarlayabileceği profil anahtarları (sıra sistem bloğunda korunur).
pub const PROFILE_KEYS: &[&str] = &["project_name", "stack", "language", "conventions", "goal"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    Global,
}

/// Tüm projelerde geçerli hafıza: `~/.evren/memory.json`.
pub fn global_memory_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".evren")
        .join("memory.json")
}

pub fn memory_path(workspace_root: &Path, scope: Scope) -> PathBuf {
    match scope {
        Scope::Global => global_memory_path(),
        Scope::Project => get_evren_dir(workspace_root).join("memory.json"),
    }
}

fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string()
}

fn empty_state() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("schema_version".into(), json!(SCHEMA_VERSION));
    m.insert("profile".into(), json!({}));
    m.insert("notes".into(), json!([]));
    m.insert("updated_at".into(), json!(now()));
    m
}

/// Hafıza deposu; dosya yoksa ya da bozuksa boş durum.
pub fn load_memory(workspace_root: &Path, scope: Scope) -> Map<String, Value> {
    load_from(&memory_path(workspace_root, scope))
}

fn load_from(path: &Path) -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return empty_state();
    };
    let Ok(Value::Object(data)) =
        serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}'))
    else {
        return empty_state();
    };
    let mut state = empty_state();
    if let Some(p @ Value::Object(_)) = data.get("profile") {
        state.insert("profile".into(), p.clone());
    }
    if let Some(n @ Value::Array(_)) = data.get("notes") {
        state.insert("notes".into(), n.clone());
    }
    if let Some(v) = data.get("schema_version") {
        state.insert("schema_version".into(), v.clone());
    }
    if let Some(v) = data.get("updated_at") {
        state.insert("updated_at".into(), v.clone());
    }
    state
}

/// Depoyu atomik yazar ve yolunu döndürür.
pub fn save_memory(
    state: &Map<String, Value>,
    workspace_root: &Path,
    scope: Scope,
) -> std::io::Result<PathBuf> {
    let path = memory_path(workspace_root, scope);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut state = state.clone();
    state.insert("schema_version".into(), json!(SCHEMA_VERSION));
    state.insert("updated_at".into(), json!(now()));
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&Value::Object(state)).unwrap_or_default();
    std::fs::write(&tmp, format!("{body}\n"))?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn profile(state: &Map<String, Value>) -> Map<String, Value> {
    state
        .get("profile")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

pub fn notes(state: &Map<String, Value>) -> Vec<Value> {
    state
        .get("notes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Profil alanını ayarlar (bilinmeyen anahtarlar da saklanır).
pub fn set_profile(
    key: &str,
    value: &str,
    workspace_root: &Path,
    scope: Scope,
) -> Result<Map<String, Value>, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("Profil anahtarı boş olamaz.".into());
    }
    let mut state = load_memory(workspace_root, scope);
    let mut prof = profile(&state);
    prof.insert(key.into(), json!(value.trim()));
    state.insert("profile".into(), Value::Object(prof));
    save_memory(&state, workspace_root, scope).map_err(|e| e.to_string())?;
    Ok(state)
}

/// Zaman damgalı not ekler; aynı metin zaten varsa dokunmaz.
pub fn add_note(
    text: &str,
    workspace_root: &Path,
    scope: Scope,
) -> Result<Map<String, Value>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Not boş olamaz.".into());
    }
    let mut state = load_memory(workspace_root, scope);
    let mut list = notes(&state);
    if list
        .iter()
        .any(|n| n.get("text").and_then(Value::as_str) == Some(text))
    {
        return Ok(state);
    }
    list.push(json!({"text": text, "at": now()}));
    state.insert("notes".into(), Value::Array(list));
    save_memory(&state, workspace_root, scope).map_err(|e| e.to_string())?;
    Ok(state)
}

/// Proje deposunu boş duruma sıfırlar.
pub fn clear_memory(workspace_root: &Path) -> Map<String, Value> {
    let state = empty_state();
    let _ = save_memory(&state, workspace_root, Scope::Project);
    state
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}

fn block(title: &str, state: &Map<String, Value>, max_notes: usize) -> Vec<String> {
    let prof = profile(state);
    let list = notes(state);
    if prof.is_empty() && list.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![title.to_string()];
    for key in PROFILE_KEYS {
        if let Some(v) = prof.get(*key).filter(|v| truthy(v)) {
            lines.push(format!("  {key}: {}", value_text(v)));
        }
    }
    for (key, v) in &prof {
        if !PROFILE_KEYS.contains(&key.as_str()) && truthy(v) {
            lines.push(format!("  {key}: {}", value_text(v)));
        }
    }
    if !list.is_empty() {
        lines.push("  Notlar:".into());
        let start = list.len().saturating_sub(max_notes);
        for n in &list[start..] {
            let text = n.get("text").map(value_text).unwrap_or_default();
            lines.push(format!("    - {text}"));
        }
    }
    lines
}

/// Sistem istemine eklenen hafıza bloğu: genel + proje; ikisi de boşsa "".
pub fn memory_context_block(workspace_root: &Path, max_notes: usize) -> String {
    let mut lines = block(
        "GENEL HAFIZA (tüm projeler):",
        &load_memory(workspace_root, Scope::Global),
        max_notes,
    );
    lines.extend(block(
        "PROJE HAFIZASI (önceki oturumlardan):",
        &load_memory(workspace_root, Scope::Project),
        max_notes,
    ));
    lines.join("\n")
}

/// Yalnız proje kapsamının bloğu (testler ve genel hafızadan bağımsız kullanım).
pub fn project_block(workspace_root: &Path, max_notes: usize) -> String {
    block(
        "PROJE HAFIZASI (önceki oturumlardan):",
        &load_memory(workspace_root, Scope::Project),
        max_notes,
    )
    .join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn empty_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = load_memory(dir.path(), Scope::Project);
        assert!(profile(&state).is_empty());
        assert!(notes(&state).is_empty());
        assert_eq!(project_block(dir.path(), 10), "");
    }

    #[test]
    fn set_profile_and_note_persist() {
        let dir = tempfile::tempdir().unwrap();
        set_profile("stack", "Python/Rich", dir.path(), Scope::Project).unwrap();
        add_note("Testler pytest ile koşar", dir.path(), Scope::Project).unwrap();
        add_note("Testler pytest ile koşar", dir.path(), Scope::Project).unwrap();
        let state = load_memory(dir.path(), Scope::Project);
        assert_eq!(profile(&state)["stack"], "Python/Rich");
        assert_eq!(notes(&state).len(), 1);
        let block = project_block(dir.path(), 10);
        assert!(block.contains("PROJE HAFIZASI"));
        assert!(block.contains("stack: Python/Rich"));
        assert!(block.contains("- Testler pytest ile koşar"));
        let raw = std::fs::read_to_string(memory_path(dir.path(), Scope::Project)).unwrap();
        assert!(raw.starts_with("{\n  \"schema_version\": 1,"));
        assert!(raw.ends_with("}\n"));
    }

    #[test]
    fn empty_key_and_note_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(set_profile(" ", "x", dir.path(), Scope::Project).is_err());
        assert!(add_note("  ", dir.path(), Scope::Project).is_err());
    }

    #[test]
    fn corrupt_file_is_empty_and_clear_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = memory_path(dir.path(), Scope::Project);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();
        assert!(profile(&load_memory(dir.path(), Scope::Project)).is_empty());
        set_profile("goal", "x", dir.path(), Scope::Project).unwrap();
        clear_memory(dir.path());
        assert!(profile(&load_memory(dir.path(), Scope::Project)).is_empty());
    }

    #[test]
    fn notes_capped_and_extra_keys_kept() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..15 {
            add_note(&format!("not {i}"), dir.path(), Scope::Project).unwrap();
        }
        set_profile("custom", "değer", dir.path(), Scope::Project).unwrap();
        let block = project_block(dir.path(), 10);
        assert!(!block.contains("- not 4\n"));
        assert!(block.contains("- not 14"));
        assert!(block.contains("custom: değer"));
    }
}
