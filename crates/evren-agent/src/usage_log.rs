//! Token kullanım günlüğü `.evren/logs/usage.jsonl` (Python `usage_log.py`).

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use evren_core::config::get_evren_dir;
use serde_json::{json, Map, Value};

pub const USAGE_FILE_NAME: &str = "usage.jsonl";
pub const ARCHIVE_FILE_NAME: &str = "usage.jsonl.1";
/// Etkin günlük bu kadar satıra ulaşınca döndürülür.
pub const MAX_LINES: usize = 5000;

/// Okunurken korunan alanlar; başka her şey atılır.
pub const FIELDS: [&str; 8] = [
    "ts",
    "model",
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "cached_tokens",
    "step",
    "mode",
];

fn line_counts() -> &'static Mutex<HashMap<PathBuf, usize>> {
    static COUNTS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn usage_log_path(workspace_root: &Path) -> PathBuf {
    get_evren_dir(workspace_root)
        .join("logs")
        .join(USAGE_FILE_NAME)
}

/// `%Y-%m-%dT%H:%M:%SZ` (UTC).
pub fn stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Python `_as_int`: bool/None → None; tam sayı; tam değerli float; rakamlı dize.
pub fn as_int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && f.is_finite())
                    .map(|f| f as i64)
            }
        }
        Value::String(s) => {
            let t = s.trim();
            if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
                t.parse().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

fn opt(v: Option<i64>) -> Value {
    v.map(Value::from).unwrap_or(Value::Null)
}

fn head(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// Ham `usage` nesnesinden beyaz listeli kayıt.
pub fn build_record(
    usage: Option<&Value>,
    model: &str,
    step: Option<i64>,
    mode: &str,
    ts: Option<&str>,
    latency_ms: Option<i64>,
) -> Map<String, Value> {
    let field = |name: &str| usage.and_then(|u| u.get(name));
    let prompt = as_int(field("prompt_tokens"));
    let completion = as_int(field("completion_tokens"));
    let mut total = as_int(field("total_tokens"));
    if total.is_none() {
        if let (Some(p), Some(c)) = (prompt, completion) {
            total = Some(p + c);
        }
    }
    let cached = field("prompt_tokens_details")
        .and_then(Value::as_object)
        .and_then(|d| as_int(d.get("cached_tokens")));
    let mut record = Map::new();
    record.insert(
        "ts".into(),
        Value::String(ts.map(str::to_string).unwrap_or_else(stamp)),
    );
    record.insert("model".into(), Value::String(head(model, 64).to_string()));
    record.insert("prompt_tokens".into(), opt(prompt));
    record.insert("completion_tokens".into(), opt(completion));
    record.insert("total_tokens".into(), opt(total));
    record.insert("cached_tokens".into(), opt(cached));
    record.insert("step".into(), opt(step));
    record.insert("mode".into(), Value::String(head(mode, 16).to_string()));
    record.insert("latency_ms".into(), opt(latency_ms));
    record
}

fn rotate(path: &Path) {
    let archive = path.with_file_name(ARCHIVE_FILE_NAME);
    if archive.exists() {
        let _ = std::fs::remove_file(&archive);
    }
    let _ = std::fs::rename(path, &archive);
    if let Ok(mut counts) = line_counts().lock() {
        counts.insert(path.to_path_buf(), 0);
    }
}

fn count_lines(path: &Path) -> usize {
    std::fs::File::open(path)
        .map(|f| BufReader::new(f).lines().count())
        .unwrap_or(0)
}

/// Bir kullanım satırı ekler; kullanılabilir alan yoksa `None`.
pub fn log_usage(
    usage: Option<&Value>,
    model: &str,
    step: Option<i64>,
    mode: &str,
    workspace_root: &Path,
    latency_ms: Option<i64>,
) -> Option<Map<String, Value>> {
    let record = build_record(usage, model, step, mode, None, latency_ms);
    if ["prompt_tokens", "completion_tokens", "total_tokens"]
        .iter()
        .all(|k| record.get(*k).is_none_or_null())
    {
        return None;
    }
    let path = usage_log_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let current = {
        let mut counts = line_counts().lock().ok()?;
        *counts
            .entry(path.clone())
            .or_insert_with(|| count_lines(&path))
    };
    if current >= MAX_LINES {
        rotate(&path);
    }
    let line = format!("{}\n", py_dumps(&Value::Object(record.clone())));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    file.write_all(line.as_bytes()).ok()?;
    if let Ok(mut counts) = line_counts().lock() {
        *counts.entry(path).or_insert(0) += 1;
    }
    Some(record)
}

/// Python `json.dumps(..., ensure_ascii=False)` varsayılan ayraçlarıyla.
pub fn py_dumps(value: &Value) -> String {
    let mut out = String::new();
    write_py(value, &mut out);
    out
}

fn write_py(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push_str(": ");
                write_py(v, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py(v, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

trait NoneOrNull {
    fn is_none_or_null(&self) -> bool;
}

impl NoneOrNull for Option<&Value> {
    fn is_none_or_null(&self) -> bool {
        matches!(self, None | Some(Value::Null))
    }
}

/// Etkin günlüğü okur (en yeni sonda); bozuk satırlar atlanır.
pub fn read_usage(workspace_root: &Path, limit: Option<usize>) -> Vec<Map<String, Value>> {
    let path = usage_log_path(workspace_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut records: Vec<Map<String, Value>> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(Value::Object(data)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let mut rec = Map::new();
        for name in FIELDS {
            rec.insert(name.into(), data.get(name).cloned().unwrap_or(Value::Null));
        }
        records.push(rec);
    }
    match limit {
        Some(n) if n < records.len() => records.split_off(records.len() - n),
        _ => records,
    }
}

fn ints<'a>(records: &'a [Map<String, Value>], key: &'a str) -> impl Iterator<Item = i64> + 'a {
    records
        .iter()
        .filter_map(move |r| r.get(key).and_then(Value::as_i64))
}

/// Toplamlar ve son `recent` isteğin ortalaması.
pub fn usage_summary(records: &[Map<String, Value>], recent: usize) -> Value {
    let prompts: Vec<i64> = ints(records, "prompt_tokens").collect();
    let completions: i64 = ints(records, "completion_tokens").sum();
    let totals: i64 = ints(records, "total_tokens").sum();
    let cached: i64 = ints(records, "cached_tokens").sum();
    let prompt_sum: i64 = prompts.iter().sum();
    let tail = if prompts.len() > recent {
        &prompts[prompts.len() - recent..]
    } else {
        &prompts[..]
    };
    let ratio = if prompt_sum > 0 {
        (cached as f64 / prompt_sum as f64 * 1000.0).round() / 1000.0
    } else {
        0.0
    };
    let avg = if tail.is_empty() {
        0
    } else {
        (tail.iter().sum::<i64>() as f64 / tail.len() as f64).round() as i64
    };
    json!({
        "cached_tokens": cached,
        "cache_ratio": ratio,
        "requests": records.len(),
        "prompt_tokens": prompt_sum,
        "completion_tokens": completions,
        "total_tokens": totals,
        "avg_prompt_tokens_recent": avg,
        "recent_window": tail.len().min(recent),
    })
}

/// Satır sayısı önbelleğini boşaltır (testler için).
pub fn reset_cache() {
    if let Ok(mut counts) = line_counts().lock() {
        counts.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ints_and_records() {
        assert_eq!(as_int(Some(&json!(true))), None);
        assert_eq!(as_int(Some(&json!(4.0))), Some(4));
        assert_eq!(as_int(Some(&json!(4.5))), None);
        assert_eq!(as_int(Some(&json!(" 12 "))), Some(12));
        assert_eq!(as_int(Some(&json!("x"))), None);
        let usage = json!({"prompt_tokens": 10, "completion_tokens": 5,
            "prompt_tokens_details": {"cached_tokens": 3}});
        let rec = build_record(Some(&usage), "m", Some(2), "ask", Some("T"), Some(7));
        assert_eq!(
            py_dumps(&Value::Object(rec)),
            "{\"ts\": \"T\", \"model\": \"m\", \"prompt_tokens\": 10, \"completion_tokens\": 5, \"total_tokens\": 15, \"cached_tokens\": 3, \"step\": 2, \"mode\": \"ask\", \"latency_ms\": 7}"
        );
    }

    #[test]
    fn log_read_summary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(log_usage(Some(&json!({})), "m", None, "", root, None).is_none());
        let usage = json!({"prompt_tokens": 100, "completion_tokens": 10,
            "prompt_tokens_details": {"cached_tokens": 50}});
        log_usage(Some(&usage), "m", Some(1), "normal", root, Some(5)).unwrap();
        log_usage(Some(&usage), "m", Some(2), "normal", root, Some(5)).unwrap();
        let recs = read_usage(root, None);
        assert_eq!(recs.len(), 2);
        assert!(recs[0].get("latency_ms").is_none());
        let s = usage_summary(&recs, 10);
        assert_eq!(s["prompt_tokens"], 200);
        assert_eq!(s["cache_ratio"], 0.5);
        assert_eq!(s["avg_prompt_tokens_recent"], 100);
        assert_eq!(s["recent_window"], 2);
        assert_eq!(read_usage(root, Some(1)).len(), 1);
        reset_cache();
    }
}
