//! `/agent`, `/agents` ve `/team` yardımcıları (Python `agent_library.py`).

use evren_agent::agent_library::{
    get_agent, load_agents, load_settings, save_settings, valid_name, AGENT_TEMPLATE, SOURCES,
};
use evren_agent::AgentSession;
use serde_json::Value;

use crate::ui::{print_error, print_info, print_success};

/// `/team` adımları: (ajan, istem kalıbı).
pub const TEAM_STEPS: [(&str, &str); 3] = [
    (
        "product-owner",
        "Şu görevi analiz et; kabul kriterleri ve kısa bir uygulama planı üret (kod değiştirme):\n{t}",
    ),
    (
        "fullstack",
        "Görev: {t}\nPlanı uygula ve değişikliği doğrula. Son yanıtın kısa bir özet olsun.",
    ),
    (
        "tester",
        "Görev: {t}\nYapılan değişikliği test et/doğrula ve sonucu kısa raporla.",
    ),
];

fn setting_text(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// `/agent [ad | kapat]`: oturumun ana ajanını gösterir/ayarlar.
pub fn handle_agent(arg: &str, session: &mut AgentSession) {
    let root = session.workspace_root().to_path_buf();
    let arg = arg.trim();
    let mut settings = load_settings(&root);
    if arg.is_empty() {
        match setting_text(settings.get("agent")) {
            Some(cur) => print_info(&format!("Ana ajan: {cur}")),
            None => print_info("Ana ajan ayarlanmamış. Ayarlamak için: /agent <ad>"),
        }
        return;
    }
    if matches!(arg.to_lowercase().as_str(), "kapat" | "off" | "yok") {
        settings.remove("agent");
        let _ = save_settings(&root, &settings);
        session.refresh_context();
        print_success("Ana ajan kapatıldı.");
        return;
    }
    if get_agent(arg, &root, session.config()).is_none() {
        print_error(&format!("Ajan bulunamadı: {arg}. Liste için /agents"));
        return;
    }
    let name = arg.trim_start_matches('@').to_string();
    settings.insert("agent".into(), Value::from(name.clone()));
    let _ = save_settings(&root, &settings);
    session.refresh_context();
    print_success(&format!("Ana ajan: {name}"));
}

/// `/agents [list | show <ad> | new <ad> | reload]`.
pub fn handle_agents(arg: &str, session: &mut AgentSession) {
    let root = session.workspace_root().to_path_buf();
    let parts: Vec<&str> = arg.split_whitespace().collect();
    let sub = parts
        .first()
        .map(|p| p.to_lowercase())
        .unwrap_or_else(|| "list".into());
    match sub.as_str() {
        "list" | "liste" => {
            let agents = load_agents(&root, session.config());
            for source in SOURCES {
                let group: Vec<_> = agents.values().filter(|a| a.source == source).collect();
                if group.is_empty() {
                    continue;
                }
                print_info(&format!("[{source}]"));
                for a in group {
                    println!("  {:<14} {}", a.name, a.description);
                }
            }
        }
        "show" | "göster" | "goster" if parts.len() > 1 => {
            let Some(a) = get_agent(parts[1], &root, session.config()) else {
                print_error(&format!("Ajan bulunamadı: {}", parts[1]));
                return;
            };
            let model = if a.model.is_empty() {
                "oturum"
            } else {
                &a.model
            };
            print_info(&format!(
                "{} [{}] model={model} adım={}",
                a.name, a.source, a.max_steps
            ));
            let tools = match &a.tools {
                Some(t) if !t.is_empty() => t.join(", "),
                _ => "tümü".to_string(),
            };
            println!("Araçlar: {tools}\n{}", a.prompt);
        }
        "new" | "yeni" if parts.len() > 1 => {
            let name = parts[1];
            if !valid_name(name) {
                print_error("Geçersiz ajan adı.");
                return;
            }
            let path = root
                .join(".evren")
                .join("agents")
                .join(format!("{name}.md"));
            if path.exists() {
                print_error(&format!("Zaten var: {}", path.display()));
                return;
            }
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, AGENT_TEMPLATE.replace("{name}", name)));
            match written {
                Ok(()) => print_success(&format!("Şablon oluşturuldu: {}", path.display())),
                Err(e) => print_error(&e.to_string()),
            }
        }
        "reload" | "yenile" => {
            print_success(&format!(
                "{} ajan yüklendi.",
                load_agents(&root, session.config()).len()
            ));
            session.refresh_context();
        }
        _ => print_info("Kullanım: /agents [show <ad> | new <ad> | reload]"),
    }
}

/// Ajanı geçici ana ajan yapar; önceki değeri döndürür (`None` = yoktu).
pub fn push_agent(session: &mut AgentSession, name: &str) -> Option<Value> {
    let root = session.workspace_root().to_path_buf();
    let mut settings = load_settings(&root);
    let previous = settings.get("agent").cloned();
    settings.insert("agent".into(), Value::from(name));
    let _ = save_settings(&root, &settings);
    session.refresh_context();
    previous
}

/// `push_agent` öncesindeki ana ajanı geri yükler.
pub fn pop_agent(session: &mut AgentSession, previous: Option<Value>) {
    let root = session.workspace_root().to_path_buf();
    let mut settings = load_settings(&root);
    match previous.filter(|p| setting_text(Some(p)).is_some()) {
        Some(p) => {
            settings.insert("agent".into(), p);
        }
        None => {
            settings.remove("agent");
        }
    }
    let _ = save_settings(&root, &settings);
    session.refresh_context();
}

/// Son dolu asistan mesajının metni (`/team` adım özeti).
pub fn last_assistant_text(session: &AgentSession) -> String {
    session
        .messages()
        .iter()
        .rev()
        .find(|m| {
            m.get("role").and_then(Value::as_str) == Some("assistant")
                && m.get("content").is_some_and(|c| match c {
                    Value::Null => false,
                    Value::String(s) => !s.is_empty(),
                    Value::Array(a) => !a.is_empty(),
                    _ => true,
                })
        })
        .and_then(|m| m.get("content"))
        .map(|c| match c {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

/// `@ajan görev` girdisi: (ad, görev).
pub fn parse_mention(input: &str) -> Option<(String, String)> {
    let rest = input.strip_prefix('@')?;
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    let name = &rest[..end];
    if !valid_name(name) {
        return None;
    }
    let after = &rest[end..];
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    let task = after.trim_start();
    if task.is_empty() {
        return None;
    }
    Some((name.to_string(), task.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn mention_parsing() {
        assert_eq!(
            parse_mention("@tester şunu dene\nikinci"),
            Some(("tester".into(), "şunu dene\nikinci".into()))
        );
        assert_eq!(parse_mention("@src/main.rs incele"), None);
        assert_eq!(parse_mention("@tester"), None);
        assert_eq!(parse_mention("@tester   "), None);
        assert_eq!(parse_mention("tester x"), None);
    }

    #[test]
    fn team_prompts() {
        assert!(TEAM_STEPS[0].1.ends_with("(kod değiştirme):\n{t}"));
        assert_eq!(
            TEAM_STEPS.map(|s| s.0),
            ["product-owner", "fullstack", "tester"]
        );
    }
}
