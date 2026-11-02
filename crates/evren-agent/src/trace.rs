//! Görev izi `.evren/logs/trace.jsonl` (Python `trace.py`, yazma tarafı).
//!
//! Hiçbir zaman hata döndürmez: bir iz satırını kaybetmek ajan turunu
//! bozmaktan iyidir.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use evren_core::config::get_evren_dir;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};

use crate::usage_log;

pub const TRACE_FILE_NAME: &str = "trace.jsonl";
pub const ARCHIVE_FILE_NAME: &str = "trace.jsonl.1";
/// Görev başlarken etkin günlük bundan büyükse `.1`'e taşınır.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const PREVIEW_CHARS: usize = 120;
pub const ARGS_CHARS: usize = 160;
const ARG_VALUE_CHARS: usize = 80;
const SCRUB_CAP: usize = 4000;
const SIGNATURE_CAP: usize = 200_000;

/// Gizli değerleri maskeleyen geri çağrı (kasa).
pub type Masker = Arc<dyn Fn(&str) -> String + Send + Sync>;

fn error_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        Regex::new(r"^\s*(?:HATA|Hata|hata|error|Error|ERROR|İPTAL|REDDEDİLDİ|TEKRAR ENGELLENDİ)\b")
            .unwrap()
    })
}

pub fn outcome_label(outcome: &str) -> &'static str {
    match outcome {
        "ok" => "tamam",
        "error" => "hata",
        "max_steps" => "adım sınırı",
        "interrupted" => "iptal",
        "exception" => "beklenmeyen hata",
        "stuck" => "takıldı (tekrar)",
        "budget" => "bütçe aşıldı",
        _ => "",
    }
}

/// `.evren/logs/trace.jsonl`; `EVREN_TRACE_FILE` doluysa o yol.
pub fn trace_path(workspace_root: &Path) -> PathBuf {
    if let Ok(over) = std::env::var("EVREN_TRACE_FILE") {
        let over = over.trim();
        if !over.is_empty() {
            return PathBuf::from(over);
        }
    }
    get_evren_dir(workspace_root)
        .join("logs")
        .join(TRACE_FILE_NAME)
}

fn head(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// Python `str(value)`.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

fn scrub(masker: Option<&Masker>, text: &str, cap: usize) -> String {
    let text = head(text, cap);
    match masker {
        Some(m) => m(text),
        None => text.to_string(),
    }
}

/// Maskelenmiş, tek satırlık, en çok `limit` karakter.
pub fn clean(masker: Option<&Masker>, text: &str, limit: usize) -> String {
    let text = scrub(masker, text, SCRUB_CAP)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() > limit {
        let mut s = head(&text, limit.saturating_sub(1)).to_string();
        s.push('…');
        s
    } else {
        text
    }
}

/// Kısa `anahtar=değer` özeti (en çok [`ARGS_CHARS`]).
pub fn summarize_args(masker: Option<&Masker>, args: &Value) -> String {
    let Some(obj) = args.as_object() else {
        return clean(masker, &py_str(args), ARGS_CHARS);
    };
    let parts: Vec<String> = obj
        .iter()
        .map(|(key, value)| {
            let shown = match value {
                Value::Array(a) => format!("[{}]", a.len()),
                Value::Object(o) => format!("{{{}}}", o.len()),
                other => clean(masker, &py_str(other), ARG_VALUE_CHARS),
            };
            if key == "command" {
                shown
            } else {
                format!("{key}={shown}")
            }
        })
        .collect();
    clean(masker, &parts.join(" "), ARGS_CHARS)
}

pub fn sort_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                if let Some(v) = map.get(k) {
                    out.insert(k.clone(), sort_keys(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

/// Aynı çağrıların tekrarını saymak için kısa sha1 (10 hex).
pub fn signature(masker: Option<&Masker>, name: &str, args: &Value) -> String {
    let raw = format!("{name}:{}", usage_log::py_dumps(&sort_keys(args)));
    let masked = scrub(masker, &raw, SIGNATURE_CAP);
    let digest = Sha1::digest(masked.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    head(&hex, 10).to_string()
}

/// Görev sonucunu makine okunur sonuca eşler.
pub fn classify_outcome(result: Option<&str>) -> &'static str {
    match result {
        None => "exception",
        Some(r) if r.starts_with("İPTAL") => "interrupted",
        Some(r) if r.starts_with("HATA: STUCK") => "stuck",
        Some(r) if r.starts_with("HATA: BÜTÇE") => "budget",
        Some(r) if r.starts_with("HATA: Ajan maksimum adım") => "max_steps",
        Some(r) if r.starts_with("HATA") || r.starts_with("Hata") => "error",
        Some(_) => "ok",
    }
}

fn usage_fields(usage: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let record = usage.map(|u| usage_log::build_record(Some(u), "", None, "", Some(""), None));
    for name in ["prompt_tokens", "cached_tokens", "completion_tokens"] {
        let v = record
            .as_ref()
            .and_then(|r| r.get(name))
            .cloned()
            .unwrap_or(Value::Null);
        out.insert(name.into(), v);
    }
    out
}

fn rotate_if_needed(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_BYTES {
        return;
    }
    let archive = path.with_file_name(ARCHIVE_FILE_NAME);
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::rename(path, &archive);
}

/// Bir ajan görevinin iz kayıtlarını ekler.
pub struct TaskTrace {
    pub enabled: bool,
    pub path: PathBuf,
    pub task_id: String,
    /// Şimdiye kadarki model istekleri (başarısız denemeler dahil).
    pub step: u32,
    pub requests: u32,
    /// Yanıt dönen istekler.
    pub responses: u32,
    started: Instant,
    ended: bool,
    model: String,
    masker: Option<Masker>,
}

impl TaskTrace {
    pub fn new(workspace_root: &Path, enabled: bool) -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        TaskTrace {
            enabled,
            path: trace_path(workspace_root),
            task_id: head(&id, 10).to_string(),
            step: 0,
            requests: 0,
            responses: 0,
            started: Instant::now(),
            ended: false,
            model: String::new(),
            masker: None,
        }
    }

    pub fn with_masker(mut self, masker: Masker) -> Self {
        self.masker = Some(masker);
        self
    }

    fn clean(&self, text: &str, limit: usize) -> String {
        clean(self.masker.as_ref(), text, limit)
    }

    fn write(&self, record: &Value) {
        if !self.enabled {
            return;
        }
        if let Some(parent) = self.path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        if record.get("type").and_then(Value::as_str) == Some("task_start") {
            rotate_if_needed(&self.path);
        }
        let line = format!("{record}\n");
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Serbest biçimli ek kayıt (ör. `model_fallback`); `task_id` eklenir.
    pub fn record(&self, mut record: Map<String, Value>) {
        record.insert("task_id".into(), self.task_id.clone().into());
        self.write(&Value::Object(record));
    }

    /// `role` / `task_ref` boşsa `null`; `task_ref` boşsa `EVREN_TASK_REF` okunur.
    pub fn start(&mut self, model: &str, mode: &str, text: &str, role: &str, task_ref: &str) {
        self.model = self.clean(model, 64);
        let task_ref = if task_ref.trim().is_empty() {
            std::env::var("EVREN_TASK_REF").unwrap_or_default()
        } else {
            task_ref.to_string()
        };
        let opt = |v: String| {
            if v.is_empty() {
                Value::Null
            } else {
                Value::String(v)
            }
        };
        self.write(&json!({
            "type": "task_start",
            "task_id": self.task_id,
            "ts": usage_log::stamp(),
            "model": self.model,
            "mode": self.clean(mode, 16),
            "role": opt(self.clean(role, 32)),
            "task_ref": opt(self.clean(&task_ref, 64)),
            "text_preview": self.clean(text, PREVIEW_CHARS),
        }));
    }

    /// Bir model isteği; her deneme için (başarısızlar dahil) çağrılır.
    pub fn request(
        &mut self,
        usage: Option<&Value>,
        ms: f64,
        finish_reason: Option<&str>,
        model: &str,
        error: bool,
    ) {
        self.step += 1;
        self.requests += 1;
        if !error {
            self.responses += 1;
        }
        let reason = if error {
            Value::String("error".into())
        } else {
            finish_reason
                .map(|r| Value::String(self.clean(r, 32)))
                .unwrap_or(Value::Null)
        };
        let mut record = Map::new();
        record.insert("type".into(), "request".into());
        record.insert("task_id".into(), self.task_id.clone().into());
        record.insert("step".into(), self.step.into());
        let model = if model.is_empty() {
            self.model.clone()
        } else {
            self.clean(model, 64)
        };
        record.insert("model".into(), model.into());
        record.extend(usage_fields(usage));
        record.insert("ms".into(), (ms as i64).into());
        record.insert("finish_reason".into(), reason);
        self.write(&Value::Object(record));
    }

    /// Bir araç çağrısı; `blocked` tekrar koruması reddettiğinde verilir.
    pub fn tool(&self, name: &str, args: &Value, ms: f64, result: &str, blocked: bool) {
        self.write(&json!({
            "type": "tool",
            "task_id": self.task_id,
            "step": self.step,
            "tool": self.clean(name, 48),
            "args": summarize_args(self.masker.as_ref(), args),
            "sig": signature(self.masker.as_ref(), name, args),
            "ms": ms as i64,
            "result_chars": result.chars().count(),
            "blocked": blocked,
            "ok": !blocked && !error_re().is_match(result),
        }));
    }

    /// Görevi (bir kez) kapatır; `None` kaçan bir istisna demektir.
    pub fn end(&mut self, result: Option<&str>) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.write(&json!({
            "type": "task_end",
            "task_id": self.task_id,
            "steps": self.responses,
            "requests": self.requests,
            "ms": self.started.elapsed().as_millis() as i64,
            "outcome": classify_outcome(result),
        }));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(clean(None, "  a \n b ", 10), "a b");
        assert_eq!(clean(None, "abcdef", 4), "abc…");
        let args =
            json!({"command": "ls -la", "path": "x", "items": [1, 2], "opts": {"a": 1}, "n": 3});
        assert_eq!(
            summarize_args(None, &args),
            "ls -la path=x items=[2] opts={1} n=3"
        );
        assert_eq!(summarize_args(None, &json!(null)), "None");
        assert_eq!(classify_outcome(None), "exception");
        assert_eq!(classify_outcome(Some("İPTAL: x")), "interrupted");
        assert_eq!(
            classify_outcome(Some("HATA: Ajan maksimum adım")),
            "max_steps"
        );
        assert_eq!(classify_outcome(Some("Hata")), "error");
        assert_eq!(classify_outcome(Some("tamam")), "ok");
        assert_eq!(signature(None, "a", &json!({"b": 1})).len(), 10);
        assert_eq!(
            signature(None, "a", &json!({"b": 1, "a": 2})),
            signature(None, "a", &json!({"a": 2, "b": 1}))
        );
        assert_eq!(outcome_label("max_steps"), "adım sınırı");
    }

    #[test]
    fn writes_records() {
        let dir = tempfile::tempdir().unwrap();
        let masker: Masker = Arc::new(|t: &str| t.replace("GIZLI", "***"));
        let mut tr = TaskTrace::new(dir.path(), true).with_masker(masker);
        tr.start("m", "normal", "görev GIZLI", "", "");
        tr.request(
            Some(&json!({"prompt_tokens": 5, "completion_tokens": 2})),
            12.7,
            Some("stop"),
            "",
            false,
        );
        tr.tool(
            "run_command",
            &json!({"command": "ls"}),
            3.0,
            "HATA: yok",
            false,
        );
        tr.end(Some("bitti"));
        tr.end(Some("ikinci"));
        let text = std::fs::read_to_string(tr.path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["text_preview"], "görev ***");
        assert_eq!(lines[1]["prompt_tokens"], 5);
        assert_eq!(lines[1]["cached_tokens"], Value::Null);
        assert_eq!(lines[1]["ms"], 12);
        assert_eq!(lines[2]["ok"], false);
        assert_eq!(lines[2]["args"], "ls");
        assert_eq!(lines[3]["outcome"], "ok");
        assert_eq!(lines[3]["steps"], 1);
        let disabled = TaskTrace::new(dir.path().join("yok").as_path(), false);
        disabled.tool("x", &json!({}), 0.0, "", false);
        assert!(!disabled.path.exists());
    }
}
