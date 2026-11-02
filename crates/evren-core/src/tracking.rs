//! Uygulama takibi `.evren/tasks.json` (Python `tracking.py`).
//!
//! Onaylanan her görev durum, ilerleme ve geçmişle izlenir. Sürümlü JSON,
//! atomik yazım.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::get_evren_dir;
use crate::redact::redact_text;
use crate::workflow_state::BLOCKER_ROUTES;

pub const SCHEMA_VERSION: i64 = 1;

/// Altı durum (Foreman uygulama yaşam döngüsü).
pub const STATUSES: &[&str] = &[
    "not-started",
    "in-progress",
    "blocked",
    "completed",
    "abandoned",
    "deferred",
];

/// Hâlâ ilgi bekleyen durumlar.
pub const OPEN_STATUSES: &[&str] = &["not-started", "in-progress", "blocked", "deferred"];

pub fn tasks_path(workspace_root: &Path) -> PathBuf {
    get_evren_dir(workspace_root).join("tasks.json")
}

fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string()
}

fn empty_state() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("schema_version".into(), json!(SCHEMA_VERSION));
    m.insert("items".into(), json!([]));
    m.insert("updated_at".into(), json!(now()));
    m
}

/// Görev deposu; dosya yoksa ya da bozuksa boş durum.
pub fn load_tasks(workspace_root: &Path) -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(tasks_path(workspace_root)) else {
        return empty_state();
    };
    let Ok(Value::Object(data)) =
        serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}'))
    else {
        return empty_state();
    };
    let Some(items @ Value::Array(_)) = data.get("items") else {
        return empty_state();
    };
    let mut state = empty_state();
    state.insert("items".into(), items.clone());
    if let Some(v) = data.get("schema_version") {
        state.insert("schema_version".into(), v.clone());
    }
    if let Some(v) = data.get("updated_at") {
        state.insert("updated_at".into(), v.clone());
    }
    state
}

/// Depoyu atomik yazar ve yolunu döndürür.
pub fn save_tasks(state: &Map<String, Value>, workspace_root: &Path) -> std::io::Result<PathBuf> {
    let path = tasks_path(workspace_root);
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

fn items_mut(state: &mut Map<String, Value>) -> &mut Vec<Value> {
    if !matches!(state.get("items"), Some(Value::Array(_))) {
        state.insert("items".into(), json!([]));
    }
    match state.get_mut("items") {
        Some(Value::Array(items)) => items,
        _ => unreachable!("items dizisi yukarıda kuruldu"),
    }
}

fn items(state: &Map<String, Value>) -> Vec<Value> {
    state
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn find_mut<'a>(state: &'a mut Map<String, Value>, task_id: &str) -> Option<&'a mut Value> {
    items_mut(state)
        .iter_mut()
        .find(|i| i.get("id").and_then(Value::as_str) == Some(task_id))
}

pub fn str_field<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

pub fn progress_of(item: &Value) -> i64 {
    item.get("progress").and_then(Value::as_i64).unwrap_or(0)
}

fn push_history(item: &mut Value, event: Value) {
    if !matches!(item.get("history"), Some(Value::Array(_))) {
        item["history"] = json!([]);
    }
    if let Some(Value::Array(h)) = item.get_mut("history") {
        h.push(event);
    }
}

fn save(state: &Map<String, Value>, root: &Path) -> Result<(), String> {
    save_tasks(state, root)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Yeni görev ekler; boş ya da yinelenen kimlikte hata.
pub fn add_task(
    task_id: &str,
    description: &str,
    deadline: &str,
    workspace_root: &Path,
) -> Result<Value, String> {
    let task_id = task_id.trim();
    let description = description.trim();
    if task_id.is_empty() {
        return Err("Görev kimliği (id) boş olamaz.".into());
    }
    if description.is_empty() {
        return Err("Görev açıklaması boş olamaz.".into());
    }
    let mut state = load_tasks(workspace_root);
    if find_mut(&mut state, task_id).is_some() {
        return Err(format!("'{task_id}' kimliği zaten var."));
    }
    let item = json!({
        "id": task_id,
        "description": description,
        "status": "not-started",
        "progress": 0,
        "deadline": deadline.trim(),
        "created_at": now(),
        "updated_at": now(),
        "history": [{"at": now(), "event": "created"}],
    });
    items_mut(&mut state).push(item.clone());
    save(&state, workspace_root)?;
    Ok(item)
}

/// Durum/ilerleme günceller ve geçmişe kayıt ekler.
pub fn update_task(
    task_id: &str,
    status: Option<&str>,
    progress: Option<i64>,
    note: &str,
    workspace_root: &Path,
) -> Result<Value, String> {
    let mut state = load_tasks(workspace_root);
    let Some(item) = find_mut(&mut state, task_id) else {
        return Err(format!("Görev bulunamadı: {task_id}"));
    };
    let status = status.map(|s| s.trim().to_lowercase());
    if let Some(s) = &status {
        if !STATUSES.contains(&s.as_str()) {
            return Err(format!(
                "Geçersiz durum: '{s}'. Geçerli: {}",
                STATUSES.join(", ")
            ));
        }
        item["status"] = json!(s);
        if s == "completed" {
            item["progress"] = json!(100);
        }
    }
    if let Some(p) = progress {
        let p = p.clamp(0, 100);
        item["progress"] = json!(p);
        if p == 100 && str_field(item, "status") != "completed" {
            item["status"] = json!("completed");
        } else if p > 0 && str_field(item, "status") == "not-started" {
            item["status"] = json!("in-progress");
        }
    }
    item["updated_at"] = json!(now());
    let mut event = json!({"at": now(), "event": "updated"});
    if let Some(s) = status.as_deref().filter(|s| !s.is_empty()) {
        event["status"] = json!(s);
    }
    if progress.is_some() {
        event["progress"] = item["progress"].clone();
    }
    if !note.is_empty() {
        event["note"] = json!(note.trim());
    }
    push_history(item, event);
    let out = item.clone();
    save(&state, workspace_root)?;
    Ok(out)
}

/// Görevleri listeler; `filter_by` `all`, `open` ya da bir durum.
pub fn list_tasks(filter_by: &str, workspace_root: &Path) -> Vec<Value> {
    let all = items(&load_tasks(workspace_root));
    let key = filter_by.trim().to_lowercase();
    match key.as_str() {
        "" | "all" => all,
        "open" => all
            .into_iter()
            .filter(|i| OPEN_STATUSES.contains(&str_field(i, "status")))
            .collect(),
        _ => all
            .into_iter()
            .filter(|i| str_field(i, "status") == key)
            .collect(),
    }
}

/// Kimliğe göre siler; bir şey silindiyse `true`.
pub fn remove_task(task_id: &str, workspace_root: &Path) -> Result<bool, String> {
    let mut state = load_tasks(workspace_root);
    let list = items_mut(&mut state);
    let before = list.len();
    list.retain(|i| i.get("id").and_then(Value::as_str) != Some(task_id));
    if list.len() == before {
        return Ok(false);
    }
    save(&state, workspace_root)?;
    Ok(true)
}

/// Kullanıcı onaylı engeli kaydeder; çıkarımla sınıflandırma yapılmaz.
pub fn set_blocker(
    task_id: &str,
    category: &str,
    evidence: &str,
    workspace_root: &Path,
) -> Result<Value, String> {
    let Some(route) = BLOCKER_ROUTES
        .iter()
        .find(|(k, _)| *k == category)
        .map(|(_, r)| *r)
    else {
        let names: Vec<&str> = BLOCKER_ROUTES.iter().map(|(k, _)| *k).collect();
        return Err(format!("Geçersiz engel kategorisi: {}", names.join(", ")));
    };
    if evidence.trim().is_empty() {
        return Err("Engel kanıtı boş olamaz.".into());
    }
    let mut state = load_tasks(workspace_root);
    let Some(item) = find_mut(&mut state, task_id) else {
        return Err(format!("Görev bulunamadı: {task_id}"));
    };
    if matches!(str_field(item, "status"), "completed" | "abandoned") {
        return Err("Tamamlanmış veya bırakılmış görev engellenemez.".into());
    }
    item["blocker"] = json!({
        "category": category,
        "evidence": redact_text(evidence.trim()),
        "route": route,
        "at": now(),
    });
    item["status"] = json!("blocked");
    item["updated_at"] = json!(now());
    push_history(
        item,
        json!({"at": now(), "event": "blocked", "category": category}),
    );
    let out = item.clone();
    save(&state, workspace_root)?;
    Ok(out)
}

pub fn resolve_blocker(task_id: &str, note: &str, workspace_root: &Path) -> Result<Value, String> {
    if note.trim().is_empty() {
        return Err("Çözüm notu boş olamaz.".into());
    }
    let mut state = load_tasks(workspace_root);
    let Some(item) = find_mut(&mut state, task_id) else {
        return Err(format!("Görev bulunamadı: {task_id}"));
    };
    if str_field(item, "status") != "blocked" {
        return Err("Görev engellenmiş durumda değil.".into());
    }
    if let Some(obj) = item.as_object_mut() {
        obj.remove("blocker");
    }
    item["status"] = json!("in-progress");
    item["updated_at"] = json!(now());
    push_history(
        item,
        json!({"at": now(), "event": "unblocked", "note": redact_text(note)}),
    );
    let out = item.clone();
    save(&state, workspace_root)?;
    Ok(out)
}

/// Durum başına sayım ve genel tamamlanma yüzdesi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressSummary {
    pub total: usize,
    pub counts: Vec<(&'static str, usize)>,
    pub percent: i64,
}

pub fn progress_summary(workspace_root: &Path) -> ProgressSummary {
    let all = items(&load_tasks(workspace_root));
    let mut counts: Vec<(&'static str, usize)> = STATUSES.iter().map(|s| (*s, 0)).collect();
    for item in &all {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("not-started");
        if let Some(c) = counts.iter_mut().find(|(s, _)| *s == status) {
            c.1 += 1;
        }
    }
    let total = all.len();
    let completed = counts
        .iter()
        .find(|(s, _)| *s == "completed")
        .map_or(0, |c| c.1);
    let percent = if total == 0 {
        0
    } else {
        // Python round(): yarımlar çifte yuvarlanır.
        let exact = completed as f64 * 100.0 / total as f64;
        let r = exact.round();
        let r = if (exact - exact.trunc() - 0.5).abs() < f64::EPSILON && r % 2.0 != 0.0 {
            r - 1.0
        } else {
            r
        };
        r as i64
    };
    ProgressSummary {
        total,
        counts,
        percent,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn add_update_list_remove() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        add_task("T1", "Birinci", "", root).unwrap();
        assert!(add_task("T1", "x", "", root)
            .unwrap_err()
            .contains("zaten var"));
        assert!(add_task(" ", "x", "", root).is_err());
        add_task("T2", "İkinci", "2020-01-01", root).unwrap();
        let t = update_task("T1", None, Some(40), "not", root).unwrap();
        assert_eq!(str_field(&t, "status"), "in-progress");
        let t = update_task("T1", None, Some(150), "", root).unwrap();
        assert_eq!(progress_of(&t), 100);
        assert_eq!(str_field(&t, "status"), "completed");
        assert!(update_task("T2", Some("bogus"), None, "", root).is_err());
        assert_eq!(list_tasks("open", root).len(), 1);
        assert_eq!(list_tasks("completed", root).len(), 1);
        let s = progress_summary(root);
        assert_eq!((s.total, s.percent), (2, 50));
        assert!(remove_task("T2", root).unwrap());
        assert!(!remove_task("T2", root).unwrap());
    }

    #[test]
    fn blockers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        add_task("T1", "Bir", "", root).unwrap();
        assert!(set_blocker("T1", "nope", "x", root).is_err());
        let t = set_blocker("T1", "auth", "token=abc123", root).unwrap();
        assert_eq!(str_field(&t, "status"), "blocked");
        assert!(t["blocker"]["evidence"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]"));
        assert!(resolve_blocker("T1", "", root).is_err());
        let t = resolve_blocker("T1", "çözüldü", root).unwrap();
        assert_eq!(str_field(&t, "status"), "in-progress");
        assert!(t.get("blocker").is_none());
        assert!(resolve_blocker("T1", "x", root).is_err());
    }

    #[test]
    fn progress_rounds_half_to_even() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..8 {
            add_task(&format!("T{i}"), "x", "", root).unwrap();
        }
        // 1/8 = 12.5 → 12 (Python round)
        update_task("T0", Some("completed"), None, "", root).unwrap();
        assert_eq!(progress_summary(root).percent, 12);
    }
}
