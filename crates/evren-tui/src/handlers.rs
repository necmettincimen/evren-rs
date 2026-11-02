//! Bilgi ve ayar komutları (Python `repl.py` `_cmd_effort`, `_cmd_models`,
//! `_cmd_quota`, `_cmd_terms`, `_cmd_tokens`, `_cmd_steps`).

use evren_agent::usage_log::{read_usage, usage_summary};
use evren_agent::AgentSession;
use evren_api::{ensure_terms_for_keys, mask_key, ApiClient, TermsUi};
use evren_core::models::recommendation;
use serde_json::{Map, Value};

use crate::ui::{print_error, print_info, print_success, print_warning, thousands};

const EFFORT_DEFAULT: &str = "none (sunucu varsayılanı)";

/// `/effort [low|medium|high|none]`.
pub fn effort(session: &AgentSession, arg: &str) {
    let client = session.client();
    let value = arg.trim().to_lowercase();
    if value.is_empty() {
        let current = client
            .reasoning_effort()
            .unwrap_or_else(|| EFFORT_DEFAULT.to_string());
        print_info(&format!("Aktif Düşünme Yoğunluğu: {current}"));
        println!("  • low    : En hızlı, koda en çok token (önerilen)");
        println!("  • medium : Dengeli");
        println!("  • high   : Derin analiz (yavaş)");
        println!("  • none   : Sunucu varsayılanı");
        println!("\nDeğiştirmek için: /effort <low|medium|high|none>");
    } else if matches!(value.as_str(), "low" | "medium" | "high" | "none") {
        client.set_reasoning_effort((value != "none").then(|| value.clone()));
        let shown = client
            .reasoning_effort()
            .unwrap_or_else(|| EFFORT_DEFAULT.to_string());
        print_success(&format!("Düşünme yoğunluğu ayarlandı: {shown}"));
    } else {
        print_warning("Geçersiz değer. Kullanım: /effort <low|medium|high|none>");
    }
}

/// `/models`: sunucudaki modeller, önerilenler açıklamalı.
pub async fn models(session: &AgentSession) {
    let list = session.client().list_models().await;
    println!("\nKullanılabilir Modeller:");
    for m in list {
        let extra = recommendation(&m)
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();
        println!("  • {m}{extra}");
    }
    println!();
}

fn quota_num(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "—".to_string(),
        Some(Value::Number(n)) => n.as_i64().map(thousands).unwrap_or_else(|| n.to_string()),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn text_or_dash(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "—".to_string(),
        Some(Value::String(s)) if s.is_empty() => "—".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// Anahtar başına kota satırları (`print_key_quotas`).
pub fn key_quota_lines(rows: &[(String, bool, Map<String, Value>)]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, (masked, active, row)) in rows.iter().enumerate() {
        let marker = if *active { " ← aktif" } else { "" };
        out.push(format!("  {}. {masked:<24}{marker}", i + 1));
        if let Some(err) = row.get("error").filter(|e| !e.is_null()) {
            let err = err.as_str().map(str::to_string).unwrap_or(err.to_string());
            out.push(format!("     hata: {err}"));
            continue;
        }
        let remaining = row
            .get("remaining")
            .or_else(|| row.get("remaining_daily_tokens"));
        let mut parts = vec![
            format!("kalan={}", quota_num(remaining)),
            format!("limit={}", quota_num(row.get("limit"))),
            format!("kullanılan={}", quota_num(row.get("used"))),
            format!("reset={}", text_or_dash(row.get("reset"))),
            format!("seviye={}", text_or_dash(row.get("level"))),
        ];
        if let Some(w) = row.get("window_minutes").filter(|v| !v.is_null()) {
            parts.push(format!("pencere={}dk", quota_num(Some(w))));
        }
        if let Some(cr) = row.get("remaining_cr").filter(|v| !v.is_null()) {
            parts.push(format!("kredi={}", quota_num(Some(cr))));
        }
        out.push(format!("     {}", parts.join("  ")));
    }
    out
}

/// Havuzdaki her anahtarın kotası: `(maskeli, aktif mi, alanlar)`
/// (Python `EvrenClient.get_all_quotas`).
pub async fn key_quota_rows(client: &ApiClient) -> Vec<(String, bool, Map<String, Value>)> {
    let keys = client.with_pool(|p| p.key_values());
    let current = client.current_key();
    let mut rows = Vec::new();
    for key in &keys {
        let data = client.get_quota(Some(key)).await;
        rows.push((mask_key(key), current.as_deref() == Some(key), data));
    }
    rows
}

/// `/quota` (`/status`): havuzdaki her anahtarın kotası ve şartlar durumu.
pub async fn quota(session: &AgentSession) {
    let client = session.client();
    let rows = key_quota_rows(client).await;
    let terms = client.get_terms_status(None).await.unwrap_or(Value::Null);
    print_info(&format!("API Kota ({} anahtar):", rows.len()));
    for line in key_quota_lines(&rows) {
        println!("{line}");
    }
    let field = |name: &str| match terms.get(name) {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    };
    print_info(&format!(
        "Şartlar Durumu: v{} (Kabul: {})",
        field("current_version"),
        field("accepted")
    ));
}

/// `/terms [force]`: havuzdaki anahtarlar için şartları kabul ettirir.
pub async fn terms(session: &AgentSession, arg: &str, ui: &mut dyn TermsUi) {
    let force = matches!(
        arg.trim().to_lowercase().as_str(),
        "force" | "--force" | "-f"
    );
    let client = session.client();
    let keys = client.with_pool(|p| p.key_values());
    if keys.is_empty() {
        print_warning("Havuzda API anahtarı yok.");
    } else {
        ensure_terms_for_keys(client, &keys, true, force, ui).await;
    }
}

fn int(rep: &Value, key: &str) -> i64 {
    rep.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// `/tokens` raporunun satırları.
pub fn token_lines(rep: &Value, usage: &Value) -> Vec<String> {
    let mut out = vec![
        format!("  • Mesaj sayısı         : {}", int(rep, "messages")),
        format!(
            "  • Tahmini prompt token : {}",
            thousands(int(rep, "estimated_prompt_tokens"))
        ),
        format!(
            "  • Çıktı bütçesi        : {}",
            thousands(int(rep, "output_budget"))
        ),
        format!(
            "  • Bağlam penceresi     : {}",
            thousands(int(rep, "context_window"))
        ),
    ];
    let ratio = rep
        .get("compaction_ratio")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    out.push(format!(
        "  • Sıkıştırma eşiği     : {} (bütçe {}, pencerenin %{})",
        thousands(int(rep, "compaction_limit")),
        thousands(int(rep, "context_budget")),
        (ratio * 100.0) as i64
    ));
    out.push(format!(
        "  • Kalan bağlam         : {}",
        thousands(int(rep, "remaining_context"))
    ));
    let calibration = rep.get("calibration").cloned().unwrap_or(Value::Null);
    out.push(format!("  • Kalibrasyon oranı    : {calibration}"));
    if let Some(u) = rep
        .get("last_usage")
        .filter(|u| u.as_object().is_some_and(|m| !m.is_empty()))
    {
        out.push(format!(
            "  • Son sunucu kullanımı : prompt={}, completion={}, total={}",
            int(u, "prompt_tokens"),
            int(u, "completion_tokens"),
            int(u, "total_tokens")
        ));
    }
    let requests = int(rep, "session_requests");
    if requests > 0 {
        let totals = rep.get("session_usage").cloned().unwrap_or(Value::Null);
        out.push(format!("  • Oturum isteği       : {requests}"));
        out.push(format!(
            "  • Oturum toplamı      : prompt={}, completion={}, total={}",
            thousands(int(&totals, "prompt_tokens")),
            thousands(int(&totals, "completion_tokens")),
            thousands(int(&totals, "total_tokens"))
        ));
    }
    if int(usage, "requests") > 0 {
        let cache = usage
            .get("cache_ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        out.push(format!(
            "  • Kalıcı log (usage)  : {} istek, toplam {} token, son {} istekte ort. prompt {}, önbellek isabeti %{}",
            int(usage, "requests"),
            thousands(int(usage, "total_tokens")),
            int(usage, "recent_window"),
            thousands(int(usage, "avg_prompt_tokens_recent")),
            (cache * 100.0).round() as i64
        ));
    }
    out
}

/// `/tokens`: bağlam ve token bütçesi.
pub fn tokens(session: &AgentSession) {
    let rep = session.token_report();
    let usage = usage_summary(&read_usage(session.workspace_root(), None), 10);
    print_info("Bağlam / Token Bütçesi:");
    for line in token_lines(&rep, &usage) {
        println!("{line}");
    }
}

/// `/steps [1-100]`: oturumun adım tavanı.
pub fn steps(session: &mut AgentSession, arg: &str) {
    let default = session.config().default_max_steps();
    let arg = arg.trim();
    if arg.is_empty() {
        let current = session.max_steps().unwrap_or(default);
        print_info(&format!(
            "Adım tavanı: {current} (env varsayılanı: {default}). Değiştirmek için: /steps <1-100>"
        ));
        return;
    }
    let Ok(value) = arg.parse::<i64>() else {
        print_error("Geçersiz sayı. Örnek: /steps 20");
        return;
    };
    let Some(value) = u32::try_from(value).ok().filter(|v| (1..=100).contains(v)) else {
        print_error("Adım tavanı 1-100 arasında olmalı.");
        return;
    };
    session.set_max_steps(Some(value));
    print_info(&format!(
        "Adım tavanı bu oturum için {value}. Kalıcı değişken: set EVREN_MAX_STEPS=<n>"
    ));
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quota_rows_show_numbers_errors_and_active_marker() {
        let ok = json!({"remaining": 1500, "limit": 2000, "used": 500, "level": "pro",
                        "window_minutes": 60})
        .as_object()
        .cloned()
        .unwrap();
        let bad = json!({"error": "401"}).as_object().cloned().unwrap();
        let lines = key_quota_lines(&[
            ("sk-a***1".to_string(), true, ok),
            ("sk-b***2".to_string(), false, bad),
        ]);
        assert!(lines[0].starts_with("  1. sk-a***1") && lines[0].ends_with(" ← aktif"));
        assert_eq!(
            lines[1],
            "     kalan=1,500  limit=2,000  kullanılan=500  reset=—  seviye=pro  pencere=60dk"
        );
        assert_eq!(lines[3], "     hata: 401");
    }

    #[test]
    fn token_report_lines() {
        let rep = json!({
            "messages": 3, "estimated_prompt_tokens": 12345, "output_budget": 8192,
            "context_window": 131072, "compaction_limit": 90000, "compaction_ratio": 0.7,
            "context_budget": 100000, "remaining_context": 110000, "calibration": 1.05,
            "last_usage": null, "session_requests": 2,
            "session_usage": {"prompt_tokens": 2000, "completion_tokens": 300, "total_tokens": 2300},
        });
        let usage = json!({"requests": 0});
        let lines = token_lines(&rep, &usage);
        assert_eq!(lines[1], "  • Tahmini prompt token : 12,345");
        assert_eq!(
            lines[4],
            "  • Sıkıştırma eşiği     : 90,000 (bütçe 100,000, pencerenin %70)"
        );
        assert_eq!(lines[6], "  • Kalibrasyon oranı    : 1.05");
        assert!(lines
            .iter()
            .any(|l| l.contains("prompt=2,000, completion=300, total=2,300")));
        assert!(!lines.iter().any(|l| l.contains("Son sunucu")));
        assert!(!lines.iter().any(|l| l.contains("Kalıcı log")));
    }
}
