//! Yerel, isteğe bağlı iş akışı kalıcılığı ve gözden geçirme yardımcıları
//! (Python `workflow_state.py`): checkpoint, karar günlüğü, görev bayrakları.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::config::get_evren_dir;
use crate::redact::redact_text;
use crate::tools::Mode;

/// Engel kategorileri ve çözüm yönlendirmeleri (sıra Python ile aynı).
pub const BLOCKER_ROUTES: &[(&str, &str)] = &[
    ("dns", "DNS kaydını ve çözümleyiciyi salt-okuma ile doğrula; doğru resmi endpoint'i teyit et."),
    ("auth", "Registry/API/SSH yetkisini ve secret referansını doğrula; kullanıcıdan erişim sağlanmasını iste."),
    ("dependency", "Bağımlılık sürümlerini karşılaştır; /diagnose dependency-hell ile daralt."),
    ("knowledge", "Eksik bilgiyi salt-okuma ile keşfet; mimari karar gerekiyorsa kullanıcıya sor."),
    ("external", "Harici servis durumunu doğrula; alternatif endpoint veya erişim için kullanıcı onayı al."),
    ("permission", "İzin kapsamını doğrula; yetki veya üretim değişikliği onayı iste."),
];

pub fn blocker_categories() -> Vec<&'static str> {
    BLOCKER_ROUTES.iter().map(|(k, _)| *k).collect()
}

fn iso_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

fn secret_key(key: &str) -> bool {
    static RE: std::sync::OnceLock<Option<Regex>> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:password|passwd|pwd|secret|token|api[_-]?key|authorization)$").ok()
    })
    .as_ref()
    .is_some_and(|r| r.is_match(key))
}

/// JSON değerini özyinelemeli maskeler (Python `_redact`).
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_text(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        Value::Object(o) => {
            let mut out = Map::new();
            for (k, v) in o {
                let nv = if secret_key(k) {
                    json!("[REDACTED]")
                } else if let (true, Value::String(s)) = (k == "arguments", v) {
                    match serde_json::from_str::<Value>(s) {
                        Ok(args) => Value::String(redact_value(&args).to_string()),
                        Err(_) => Value::String(redact_text(s)),
                    }
                } else {
                    redact_value(v)
                };
                out.insert(k.clone(), nv);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let name = file_name(path);
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("İş akışı kaydı okunamadı: {name}: {e}"))?;
    let state: Value = serde_json::from_str(&text)
        .map_err(|e| format!("İş akışı kaydı okunamadı: {name}: {e}"))?;
    match state {
        Value::Object(m) if m.get("schema_version") == Some(&json!(1)) => Ok(Some(m)),
        _ => Err(format!("Geçersiz iş akışı kaydı: {name}")),
    }
}

fn write(path: &Path, state: &Value) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&redact_value(state)).unwrap_or_default();
    let result = std::fs::write(&tmp, format!("{body}\n")).and_then(|()| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    });
    if tmp.exists() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|e| e.to_string())?;
    Ok(path.to_path_buf())
}

fn resolved(root: &Path) -> String {
    let p = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let s = p.to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

pub fn checkpoint_path(root: &Path) -> PathBuf {
    get_evren_dir(root).join("checkpoint.json")
}

/// Duraklamış görevi kaydeder; sistem iletisi geri yüklemede yeniden kurulur.
pub fn save_checkpoint(
    root: &Path,
    messages: &[Value],
    mode: Mode,
    model: &str,
    frame: &Value,
    goal: &str,
) -> Result<PathBuf, String> {
    let goal = if goal.is_empty() {
        messages
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
            .and_then(|m| m.get("content").and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    } else {
        goal.to_string()
    };
    let kept: Vec<Value> = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) != Some("system"))
        .cloned()
        .collect();
    write(
        &checkpoint_path(root),
        &json!({
            "schema_version": 1, "workspace": resolved(root),
            "saved_at": iso_now(), "mode": mode.as_str(),
            "model": model, "frame": frame, "goal": goal, "messages": kept,
        }),
    )
}

fn validate_messages(messages: Option<&Value>) -> Result<(), String> {
    let Some(Value::Array(messages)) = messages else {
        return Err("Checkpoint mesaj geçmişi geçersiz.".into());
    };
    if messages.is_empty() {
        return Err("Checkpoint mesaj geçmişi geçersiz.".into());
    }
    let mut pending: HashSet<String> = HashSet::new();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        if !m.is_object() || !matches!(role, "user" | "assistant" | "tool") {
            return Err("Checkpoint mesajı geçersiz.".into());
        }
        if !m.get("content").is_some_and(Value::is_string) {
            return Err("Checkpoint mesaj içeriği geçersiz.".into());
        }
        if role == "tool" {
            let id = m.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
            if !pending.remove(id) {
                return Err("Checkpoint araç sonucu eşleşmiyor.".into());
            }
            continue;
        }
        if !pending.is_empty() {
            return Err("Checkpoint tamamlanmamış araç paketi içeriyor.".into());
        }
        let calls = match m.get("tool_calls") {
            None => Vec::new(),
            Some(Value::Array(c)) => c.clone(),
            Some(_) => return Err("Checkpoint araç çağrıları geçersiz.".into()),
        };
        for call in calls {
            let Some(id) = call.get("id").and_then(Value::as_str) else {
                return Err("Checkpoint araç kimliği geçersiz.".into());
            };
            let f = call.get("function");
            let ok = f.is_some_and(|f| {
                f.get("name").is_some_and(Value::is_string)
                    && f.get("arguments").is_some_and(Value::is_string)
            });
            if !ok {
                return Err("Checkpoint araç fonksiyonu geçersiz.".into());
            }
            pending.insert(id.to_string());
        }
    }
    if !pending.is_empty() {
        return Err("Checkpoint tamamlanmamış araç paketi içeriyor.".into());
    }
    Ok(())
}

/// Checkpoint'i doğrulayarak yükler; yoksa `None`.
pub fn load_checkpoint(root: &Path) -> Result<Option<Map<String, Value>>, String> {
    let Some(state) = read(&checkpoint_path(root))? else {
        return Ok(None);
    };
    let s = |k: &str| state.get(k).and_then(Value::as_str);
    if s("workspace") != Some(resolved(root).as_str()) {
        return Err("Checkpoint başka çalışma alanına ait.".into());
    }
    if s("mode").and_then(Mode::parse).is_none() {
        return Err("Checkpoint kip bilgisi geçersiz.".into());
    }
    if s("saved_at").is_none() {
        return Err("Checkpoint kayıt tarihi geçersiz.".into());
    }
    validate_messages(state.get("messages"))?;
    if s("model").map_or(true, str::is_empty) {
        return Err("Checkpoint model bilgisi geçersiz.".into());
    }
    if s("goal").map_or(true, str::is_empty) {
        return Err("Checkpoint hedef bilgisi geçersiz.".into());
    }
    let fields = [
        "rol",
        "cerceve",
        "brief",
        "kisit",
        "cikti",
        "clarifications",
    ];
    let frame_ok = state
        .get("frame")
        .and_then(Value::as_object)
        .is_some_and(|f| {
            f.len() == fields.len()
                && fields
                    .iter()
                    .all(|k| f.get(*k).is_some_and(Value::is_string))
        });
    if !frame_ok {
        return Err("Checkpoint kalıp bilgisi geçersiz.".into());
    }
    Ok(Some(state))
}

pub fn clear_checkpoint(root: &Path) {
    let _ = std::fs::remove_file(checkpoint_path(root));
}

fn decisions_path(root: &Path) -> PathBuf {
    get_evren_dir(root).join("decisions.json")
}

/// Onaylı karar günlüğü.
pub fn load_decisions(root: &Path) -> Result<Vec<Map<String, Value>>, String> {
    let Some(state) = read(&decisions_path(root))? else {
        return Ok(Vec::new());
    };
    let fields = ["id", "decision", "reason", "risk", "at"];
    let invalid = || "Karar günlüğü geçersiz.".to_string();
    let items = state
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let mut out = Vec::new();
    for item in items {
        let obj = item.as_object().ok_or_else(invalid)?;
        let ok = obj.len() == fields.len()
            && fields
                .iter()
                .all(|k| obj.get(*k).is_some_and(Value::is_string));
        if !ok {
            return Err(invalid());
        }
        out.push(obj.clone());
    }
    Ok(out)
}

pub fn add_decision(
    root: &Path,
    decision: &str,
    reason: &str,
    risk: &str,
) -> Result<Map<String, Value>, String> {
    if decision.trim().is_empty() || reason.trim().is_empty() || risk.trim().is_empty() {
        return Err("Karar, gerekçe ve risk alanları boş olamaz.".into());
    }
    let mut items = load_decisions(root)?;
    let item = json!({
        "id": format!("decision-{}", items.len() + 1),
        "decision": decision.trim(), "reason": reason.trim(), "risk": risk.trim(),
        "at": iso_now(),
    });
    let Value::Object(obj) = item.clone() else {
        unreachable!("json! nesnesi")
    };
    items.push(obj);
    let list: Vec<Value> = items.into_iter().map(Value::Object).collect();
    write(
        &decisions_path(root),
        &json!({"schema_version": 1, "items": list}),
    )?;
    match redact_value(&item) {
        Value::Object(o) => Ok(o),
        _ => unreachable!("nesne maskelenince nesne kalır"),
    }
}

fn clip_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Sistem bloğu için son on karar.
pub fn decision_context(root: &Path) -> Result<String, String> {
    let items = load_decisions(root)?;
    if items.is_empty() {
        return Ok(String::new());
    }
    let mut lines =
        vec!["KULLANICI ONAYLI KARAR GÜNLÜĞÜ (geçmiş bağlam; yürütme izni değildir):".to_string()];
    let start = items.len().saturating_sub(10);
    for item in &items[start..] {
        let f = |k: &str| {
            item.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        lines.push(format!(
            "- {}: {} | Gerekçe: {} | Risk: {}",
            f("id"),
            clip_chars(&f("decision"), 800),
            clip_chars(&f("reason"), 400),
            clip_chars(&f("risk"), 400)
        ));
    }
    lines.push("Yeni bilgi kararla çelişirse kullanıcıya bildir; onayı varsayma.".into());
    Ok(lines.join("\n"))
}

/// `datetime.fromisoformat` alt kümesi; saat dilimi yoksa UTC sayılır.
pub fn parse_iso(text: &str) -> Option<DateTime<Utc>> {
    let t = text.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(t) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = DateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f%:z") {
        return Some(d.with_timezone(&Utc));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(t, fmt) {
            return Some(Utc.from_utc_datetime(&n));
        }
    }
    NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|n| Utc.from_utc_datetime(&n))
}

/// Açık görevlere `flags` (blocked/overdue/stalled) ekler.
pub fn review_tasks(items: &[Value], now: Option<DateTime<Utc>>) -> Result<Vec<Value>, String> {
    let now = now.unwrap_or_else(Utc::now);
    let mut out = Vec::new();
    for item in items {
        let status = item.get("status").and_then(Value::as_str).unwrap_or("");
        if matches!(status, "completed" | "abandoned") {
            continue;
        }
        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
        let mut flags: Vec<&str> = Vec::new();
        if status == "blocked" {
            flags.push("blocked");
        }
        if let Some(deadline) = item
            .get("deadline")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
        {
            let due = parse_iso(deadline)
                .ok_or_else(|| format!("Geçersiz görev tarihi: {id}: {deadline}"))?;
            if due < now {
                flags.push("overdue");
            }
        }
        if let Some(updated) = item
            .get("updated_at")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
        {
            if matches!(status, "in-progress" | "not-started") {
                let stamp = parse_iso(updated)
                    .ok_or_else(|| format!("Geçersiz görev güncelleme tarihi: {id}"))?;
                if now - stamp >= chrono::Duration::days(14) {
                    flags.push("stalled");
                }
            }
        }
        let mut v = item.clone();
        v["flags"] = json!(flags);
        out.push(v);
    }
    Ok(out)
}

pub fn flags_of(item: &Value) -> Vec<String> {
    item.get("flags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn decisions_roundtrip_and_context() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(add_decision(root, "", "r", "k").is_err());
        let d = add_decision(root, "Postgres kullan", "password=hunter2", "kilitlenme").unwrap();
        assert_eq!(d["id"], "decision-1");
        assert!(d["reason"].as_str().unwrap().contains("[REDACTED]"));
        let ctx = decision_context(root).unwrap();
        assert!(ctx.contains("decision-1: Postgres kullan"));
        assert!(!ctx.contains("hunter2"));
    }

    #[test]
    fn checkpoint_roundtrip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(load_checkpoint(root).unwrap().is_none());
        let frame =
            json!({"rol":"","cerceve":"","brief":"b","kisit":"","cikti":"","clarifications":""});
        let msgs = vec![
            json!({"role":"system","content":"s"}),
            json!({"role":"user","content":"hedef"}),
            json!({"role":"assistant","content":"","tool_calls":[{"id":"c1","function":{"name":"x","arguments":"{\"token\":\"abc\"}"}}]}),
            json!({"role":"tool","tool_call_id":"c1","content":"ok"}),
        ];
        save_checkpoint(root, &msgs, Mode::Plan, "m", &frame, "").unwrap();
        let st = load_checkpoint(root).unwrap().unwrap();
        assert_eq!(st["goal"], "hedef");
        assert_eq!(st["messages"].as_array().unwrap().len(), 3);
        let args = st["messages"][1]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        assert!(args.contains("[REDACTED]"));
        let bad = vec![json!({"role":"tool","tool_call_id":"z","content":"x"})];
        save_checkpoint(root, &bad, Mode::Plan, "m", &frame, "g").unwrap();
        assert!(load_checkpoint(root).unwrap_err().contains("eşleşmiyor"));
        clear_checkpoint(root);
        assert!(!checkpoint_path(root).exists());
    }

    #[test]
    fn review_flags() {
        let now = parse_iso("2026-11-02T00:00:00+00:00").unwrap();
        let items = vec![
            json!({"id":"a","status":"blocked","deadline":"2026-01-01","updated_at":"2026-11-01T00:00:00+00:00"}),
            json!({"id":"b","status":"in-progress","updated_at":"2026-10-01T00:00:00"}),
            json!({"id":"c","status":"completed"}),
        ];
        let r = review_tasks(&items, Some(now)).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(flags_of(&r[0]), vec!["blocked", "overdue"]);
        assert_eq!(flags_of(&r[1]), vec!["stalled"]);
        assert!(review_tasks(&[json!({"id":"x","deadline":"yarın"})], Some(now)).is_err());
    }
}
