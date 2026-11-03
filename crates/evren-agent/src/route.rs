//! Planlayıcı/uygulayıcı yönlendirmesi (Python `agent.py` `_should_route`,
//! `_planned_step_budget`, `_missing_plan_paths_note`, `_collect_planner_reads`,
//! `_plan_to_todos`).
//!
//! Model çağrısı gerektiren `_plan_phase` ve `_prepare_phase` döngü
//! modülündedir; burada yalnızca saf yardımcılar vardır.

use std::path::Path;
use std::sync::OnceLock;

use evren_core::config::Config;
use regex::Regex;
use serde_json::{json, Value};

use crate::guards::{no_plan_re, plan_intent_re, plan_path_re};
use crate::todos::TodoStore;
use crate::token_manager::TokenManager;

/// Planlayıcı okumalarının toplam karakter bütçesi.
pub const PLANNER_READS_BUDGET: usize = 40_000;

/// Yönlendirme sonrası uygulayıcı profili: `(model, thinking)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecProfile {
    pub model: String,
    pub thinking: Option<bool>,
}

fn numbered_item_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    #[allow(clippy::expect_used)]
    RE.get_or_init(|| Regex::new(r"(?m)^\s*\d{1,2}[.)]\s").expect("sabit regex"))
}

fn todo_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    #[allow(clippy::expect_used)]
    RE.get_or_init(|| Regex::new(r"^\s*(?:\*\*)?(\d{1,2})[.)]\s+(.+)$").expect("sabit regex"))
}

/// Büyük brief'i önce planlayıcıya göndermeli mi?
pub fn should_route(config: &Config, model: &str, text: &str, tm: &TokenManager) -> bool {
    if !config.auto_route_enabled() {
        return false;
    }
    if config.planner_model(model) == config.executor_model(model) {
        return false;
    }
    if no_plan_re().is_match(text) {
        return false;
    }
    if plan_intent_re().is_match(text) {
        return true;
    }
    tm.estimate_text(text) >= config.route_min_tokens()
}

/// Plan maddelerine göre adım bütçesi: `max(max_steps, min(40, 3*madde+5))`.
pub fn planned_step_budget(
    messages: &[Value],
    max_steps: u32,
    exec_profile: Option<&ExecProfile>,
) -> u32 {
    if exec_profile.is_none() {
        return max_steps;
    }
    let Some(content) = messages
        .last()
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
    else {
        return max_steps;
    };
    let Some(idx) = content.find("UYGULAMA PLANI") else {
        return max_steps;
    };
    let items = numbered_item_re().find_iter(&content[idx..]).count() as u32;
    if items == 0 {
        return max_steps;
    }
    max_steps.max((3 * items + 5).min(40))
}

/// Planda geçen ama çalışma alanında olmayan yollar için not (yoksa boş).
pub fn missing_plan_paths_note(workspace_root: &Path, plan: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for m in plan_path_re().find_iter(plan).flatten() {
        let raw = m.as_str().to_string();
        if seen.contains(&raw) {
            continue;
        }
        seen.push(raw.clone());
        let rel = raw.replace('\\', "/");
        let rel = rel.trim_start_matches(['.', '/']);
        if rel.is_empty() {
            continue;
        }
        if !workspace_root.join(rel).exists() {
            missing.push(raw);
        }
    }
    if missing.is_empty() {
        return String::new();
    }
    let shown: Vec<&str> = missing.iter().take(12).map(String::as_str).collect();
    format!(
        "\n\n[EVREN] Planda geçen şu yollar çalışma alanında YOK (planlayıcı tahmin etmiş olabilir); okumadan önce find_files ile gerçek yolu bul: {}",
        shown.join(", ")
    )
}

/// Planlayıcının `view_file` çıktılarını bütçe dahilinde birleştirir.
pub fn collect_planner_reads(messages: &[Value], budget: usize) -> String {
    let mut read_ids: Vec<String> = Vec::new();
    for m in messages {
        if m.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(calls) = m.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            let name = call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if name == "view_file" {
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    read_ids.push(id.to_string());
                }
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0usize;
    for m in messages {
        if m.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let id = m.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
        if !read_ids.iter().any(|r| r == id) {
            continue;
        }
        let Some(content) = m.get("content").and_then(Value::as_str) else {
            continue;
        };
        if content.is_empty() || content.starts_with("HATA") || content.starts_with("BİLGİ") {
            continue;
        }
        let len = content.chars().count();
        if used + len > budget {
            continue;
        }
        used += len;
        parts.push(content.to_string());
    }
    parts.join("\n\n")
}

/// Numaralı plan maddelerini yapılacaklar listesine yazar (en az 2 madde, en çok 10).
pub fn plan_to_todos(plan: &str, store: &mut TodoStore) {
    let mut items: Vec<String> = Vec::new();
    for line in plan.lines() {
        if let Some(caps) = todo_line_re().captures(line) {
            let text = caps
                .get(2)
                .map(|m| m.as_str().trim().trim_matches('*').trim())
                .unwrap_or("");
            let clipped: String = text.chars().take(200).collect();
            items.push(clipped);
        }
    }
    if items.len() < 2 {
        return;
    }
    let raw: Vec<Value> = items
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, content)| {
            json!({
                "content": content,
                "status": if i == 0 { "in_progress" } else { "pending" },
            })
        })
        .collect();
    let _ = store.write(&raw);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn step_budget_from_plan_items() {
        let profile = ExecProfile {
            model: "m".into(),
            thinking: None,
        };
        let msgs =
            vec![json!({"role": "user", "content": "x\n\nUYGULAMA PLANI\n1. a\n2. b\n3) c"})];
        assert_eq!(planned_step_budget(&msgs, 10, Some(&profile)), 14);
        assert_eq!(planned_step_budget(&msgs, 30, Some(&profile)), 30);
        assert_eq!(planned_step_budget(&msgs, 10, None), 10);
        let none = vec![json!({"role": "user", "content": "plan yok"})];
        assert_eq!(planned_step_budget(&none, 10, Some(&profile)), 10);
    }

    #[test]
    fn missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.py"), "x").unwrap();
        let note = missing_plan_paths_note(dir.path(), "src/a.py ve src/yok.py düzenle");
        assert!(note.contains("src/yok.py"));
        assert!(!note.contains("src/a.py,"));
        assert_eq!(missing_plan_paths_note(dir.path(), "src/a.py"), "");
    }

    #[test]
    fn planner_reads_and_todos() {
        let msgs = vec![
            json!({"role": "assistant", "content": "", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "view_file", "arguments": "{}"}},
                {"id": "c2", "type": "function", "function": {"name": "run_command", "arguments": "{}"}}
            ]}),
            json!({"role": "tool", "tool_call_id": "c1", "name": "view_file", "content": "içerik"}),
            json!({"role": "tool", "tool_call_id": "c2", "name": "run_command", "content": "çıktı"}),
            json!({"role": "tool", "tool_call_id": "c1", "name": "view_file", "content": "HATA: yok"}),
        ];
        assert_eq!(collect_planner_reads(&msgs, 100), "içerik");
        assert_eq!(collect_planner_reads(&msgs, 2), "");

        let mut store = TodoStore::new();
        plan_to_todos("1. tek", &mut store);
        assert!(store.is_empty());
        plan_to_todos("1. **ilk**\n2) ikinci\nnot", &mut store);
        let items = store.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].content, "ilk");
        assert_eq!(items[0].status, "in_progress");
        assert_eq!(items[1].status, "pending");
    }
}
