//! Kota JSON + başlık normalizasyonu (`EvrenClient._normalize_quota`).

use std::collections::HashMap;

use serde_json::{Map, Value};

fn first<'a>(
    out: &'a Map<String, Value>,
    keys: &[&str],
    headers: &'a HashMap<String, String>,
    header_keys: &[&str],
) -> Option<Value> {
    for k in keys {
        if let Some(v) = out.get(*k) {
            if !v.is_null() {
                return Some(v.clone());
            }
        }
    }
    for hk in header_keys {
        if let Some(v) = headers.get(*hk) {
            if !v.is_empty() {
                return Some(Value::from(v.as_str()));
            }
        }
    }
    None
}

/// Python `int(v)`: tam sayı, kesirli sayı (kesilir) ya da sayı dizesi.
fn to_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// `_as_int`: çevrilemezse değer olduğu gibi kalır.
fn as_int(v: Value) -> Value {
    match to_i64(&v) {
        Some(i) => Value::from(i),
        None => v,
    }
}

/// Başlık adları küçük harfle beklenir.
pub fn normalize_quota(data: &Value, headers: &HashMap<String, String>) -> Map<String, Value> {
    let mut out: Map<String, Value> = data.as_object().cloned().unwrap_or_default();

    let mut remaining = first(
        &out,
        &["remaining_daily_tokens", "remaining_tokens", "remaining"],
        headers,
        &["x-evren-daily-remaining-tokens"],
    );
    let limit = first(
        &out,
        &[
            "daily_limit_tokens",
            "daily_limit",
            "daily_tokens_limit",
            "limit",
            "cap",
            "quota_limit",
        ],
        headers,
        &["x-evren-daily-limit-tokens", "x-evren-daily-limit"],
    );
    let used = first(
        &out,
        &["daily_used_tokens", "used_tokens", "used"],
        headers,
        &[],
    );
    let reset = first(
        &out,
        &["reset_at", "resets_at", "quota_reset", "reset"],
        headers,
        &["x-evren-quota-reset", "x-ratelimit-reset"],
    );
    let remaining_cr = first(&out, &["remaining_cr"], headers, &[]);
    let held_cr = first(&out, &["held_cr"], headers, &[]);

    if remaining.is_none() {
        if let (Some(l), Some(u)) = (
            limit.as_ref().and_then(to_i64),
            used.as_ref().and_then(to_i64),
        ) {
            remaining = Some(Value::from(l - u));
        }
    }

    if let Some(r) = remaining.map(as_int) {
        out.insert("remaining_daily_tokens".into(), r.clone());
        out.insert("remaining".into(), r);
    }
    if let Some(l) = limit.map(as_int) {
        out.insert("limit".into(), l);
    }
    if let Some(u) = used.map(as_int) {
        out.insert("used".into(), u);
    }
    if let Some(r) = reset {
        out.insert("reset".into(), r);
    }
    if let Some(c) = remaining_cr.map(as_int) {
        out.insert("remaining_cr".into(), c);
    }
    if let Some(c) = held_cr.map(as_int) {
        out.insert("held_cr".into(), c);
    }
    if let Some(d) = data.as_object() {
        if let Some(l) = d.get("level") {
            out.insert("level".into(), l.clone());
        }
        if let Some(w) = d.get("window_minutes") {
            out.insert("window_minutes".into(), w.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rolling_window_shape() {
        let data = json!({
            "used_tokens": 100, "cap": 1000, "window_minutes": 60,
            "reset_at": "2026-10-07T12:00:00Z", "level": "ok", "remaining_cr": 50
        });
        let q = normalize_quota(&data, &HashMap::new());
        assert_eq!(q["remaining"], 900);
        assert_eq!(q["remaining_daily_tokens"], 900);
        assert_eq!(q["limit"], 1000);
        assert_eq!(q["used"], 100);
        assert_eq!(q["reset"], "2026-10-07T12:00:00Z");
        assert_eq!(q["remaining_cr"], 50);
        assert_eq!(q["level"], "ok");
        assert_eq!(q["window_minutes"], 60);
    }

    #[test]
    fn headers_and_strings() {
        let mut h = HashMap::new();
        h.insert(
            "x-evren-daily-remaining-tokens".to_owned(),
            "12345".to_owned(),
        );
        h.insert("x-evren-daily-limit".to_owned(), "abc".to_owned());
        let q = normalize_quota(&json!({}), &h);
        assert_eq!(q["remaining_daily_tokens"], 12345);
        assert_eq!(q["limit"], "abc");
        let q2 = normalize_quota(
            &json!({"error": "x", "remaining_daily_tokens": "Bilinmiyor"}),
            &HashMap::new(),
        );
        assert_eq!(q2["remaining"], "Bilinmiyor");
        assert_eq!(q2["error"], "x");
    }
}
