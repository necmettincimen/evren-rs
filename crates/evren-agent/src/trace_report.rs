//! Görev izinin okuma tarafı (`/debug`; Python `trace.py` `read_tasks`,
//! `summarize_task`, `table_rows`, `summary_line`, `header_line`).

use std::collections::HashSet;
use std::path::Path;

use serde_json::{Map, Value};

use crate::trace::{trace_path, ARCHIVE_FILE_NAME};

pub type Record = Map<String, Value>;

/// Bozuk satırlar ve yabancı kayıtlar sessizce atlanır.
pub fn read_records(path: &Path) -> Vec<Record> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(Value::Object(m))
                    if m.get("task_id").is_some_and(Value::is_string)
                        && m.get("type").is_some_and(truthy) =>
                {
                    Some(m)
                }
                _ => None,
            }
        })
        .collect()
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Kayıtları görev kimliğine göre, her görevin ilk kaydı sırasıyla gruplar.
pub fn group_tasks(records: Vec<Record>) -> Vec<Vec<Record>> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: Vec<Vec<Record>> = Vec::new();
    for rec in records {
        let id = rec
            .get("task_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match order.iter().position(|x| *x == id) {
            Some(i) => groups[i].push(rec),
            None => {
                order.push(id);
                groups.push(vec![rec]);
            }
        }
    }
    groups
}

/// İzlenen görevler (eskiden yeniye); arşiv yalnız gerekince okunur.
pub fn read_tasks(workspace_root: &Path, count: usize) -> Vec<Vec<Record>> {
    let path = trace_path(workspace_root);
    let records = read_records(&path);
    let tasks = group_tasks(records.clone());
    if tasks.len() < count {
        let mut older = read_records(&path.with_file_name(ARCHIVE_FILE_NAME));
        if !older.is_empty() {
            older.extend(records);
            return group_tasks(older);
        }
    }
    tasks
}

fn int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

fn text(rec: &Record, key: &str) -> String {
    match rec.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(false)) => String::new(),
        Some(v) => v.to_string(),
    }
}

fn kind(rec: &Record) -> &str {
    rec.get("type").and_then(Value::as_str).unwrap_or_default()
}

/// Binlik ayraçlı tamsayı (`{n:,}`); sayı değilse "—".
pub fn fmt_int(v: Option<&Value>) -> String {
    int(v).map_or_else(|| "—".to_string(), commas)
}

pub fn commas(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaskSummary {
    pub task_id: String,
    pub ts: String,
    pub model: String,
    pub mode: String,
    pub text_preview: String,
    pub finished: bool,
    pub outcome: String,
    pub requests: usize,
    pub tool_calls: usize,
    pub prompt_tokens: Option<i64>,
    pub cache_pct: Option<i64>,
    pub repeated: usize,
    pub blocked: usize,
    pub failed: usize,
    pub total_ms: i64,
}

fn py_round(x: f64) -> i64 {
    // Python round(): yarımda çifte yuvarlar.
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 {
        (r - x.signum()) as i64
    } else {
        r as i64
    }
}

pub fn summarize_task(records: &[Record]) -> TaskSummary {
    let empty = Record::new();
    let start = records
        .iter()
        .find(|r| kind(r) == "task_start")
        .unwrap_or(&empty);
    let end = records.iter().rev().find(|r| kind(r) == "task_end");
    let requests: Vec<&Record> = records.iter().filter(|r| kind(r) == "request").collect();
    let tools: Vec<&Record> = records.iter().filter(|r| kind(r) == "tool").collect();

    let prompts: Vec<i64> = requests
        .iter()
        .filter_map(|r| int(r.get("prompt_tokens")))
        .collect();
    let prompt_total = (!prompts.is_empty()).then(|| prompts.iter().sum());
    let pairs: Vec<(i64, i64)> = requests
        .iter()
        .filter_map(|r| Some((int(r.get("prompt_tokens"))?, int(r.get("cached_tokens"))?)))
        .collect();
    let paired: i64 = pairs.iter().map(|(p, _)| p).sum();
    let cache_pct = (paired != 0).then(|| {
        let cached: i64 = pairs.iter().map(|(_, c)| c).sum();
        py_round(100.0 * cached as f64 / paired as f64)
    });

    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut repeated = 0;
    for rec in &tools {
        let sig = {
            let s = text(rec, "sig");
            if s.is_empty() {
                text(rec, "args")
            } else {
                s
            }
        };
        if !seen.insert((text(rec, "tool"), sig)) {
            repeated += 1;
        }
    }

    let total_ms = match end.and_then(|e| int(e.get("ms"))) {
        Some(ms) => ms,
        None => {
            requests
                .iter()
                .map(|r| int(r.get("ms")).unwrap_or(0))
                .sum::<i64>()
                + tools
                    .iter()
                    .filter(|r| text(r, "tool") != "delegate")
                    .map(|r| int(r.get("ms")).unwrap_or(0))
                    .sum::<i64>()
        }
    };

    TaskSummary {
        task_id: records
            .first()
            .map(|r| text(r, "task_id"))
            .unwrap_or_default(),
        ts: text(start, "ts"),
        model: text(start, "model"),
        mode: text(start, "mode"),
        text_preview: text(start, "text_preview"),
        finished: end.is_some(),
        outcome: end.map(|e| text(e, "outcome")).unwrap_or_default(),
        requests: requests.len(),
        tool_calls: tools.len(),
        prompt_tokens: prompt_total,
        cache_pct,
        repeated,
        blocked: tools
            .iter()
            .filter(|r| r.get("blocked").is_some_and(truthy))
            .count(),
        failed: tools
            .iter()
            .filter(|r| {
                r.get("ok") == Some(&Value::Bool(false)) && !r.get("blocked").is_some_and(truthy)
            })
            .count(),
        total_ms,
    }
}

/// Tablo satırı: adım, tür, özet, ms, sonuç karakteri, durum.
pub type Row = [String; 6];

pub fn table_rows(records: &[Record]) -> Vec<Row> {
    let task_model = records
        .iter()
        .find(|r| kind(r) == "task_start")
        .map(|r| text(r, "model"));
    let mut rows = Vec::new();
    for rec in records {
        let step = fmt_int(rec.get("step"));
        match kind(rec) {
            "request" => {
                let usage = match int(rec.get("prompt_tokens")) {
                    None => "kullanım bilgisi yok".to_string(),
                    Some(prompt) => {
                        let cache = match int(rec.get("cached_tokens")) {
                            Some(c) if prompt != 0 => format!(
                                " (önbellek %{})",
                                py_round(100.0 * c as f64 / prompt as f64)
                            ),
                            _ => String::new(),
                        };
                        format!(
                            "prompt {}{cache} • çıktı {}",
                            commas(prompt),
                            fmt_int(rec.get("completion_tokens"))
                        )
                    }
                };
                let model = text(rec, "model");
                let model = if Some(&model) == task_model.as_ref() {
                    String::new()
                } else {
                    model
                };
                let detail = [model, usage, text(rec, "finish_reason")]
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" • ");
                rows.push([
                    step,
                    "istek".into(),
                    detail,
                    fmt_int(rec.get("ms")),
                    String::new(),
                    String::new(),
                ]);
            }
            "tool" => {
                let status = if rec.get("blocked").is_some_and(truthy) {
                    "ENGELLENDİ"
                } else if rec.get("ok") == Some(&Value::Bool(false)) {
                    "hata"
                } else {
                    ""
                };
                let tool = text(rec, "tool");
                rows.push([
                    step,
                    if tool.is_empty() { "?".into() } else { tool },
                    text(rec, "args"),
                    fmt_int(rec.get("ms")),
                    fmt_int(rec.get("result_chars")),
                    status.into(),
                ]);
            }
            _ => {}
        }
    }
    rows
}

pub fn summary_line(s: &TaskSummary) -> String {
    let seconds = s.total_ms as f64 / 1000.0;
    let outcome = if s.finished {
        let label = crate::trace::outcome_label(&s.outcome);
        if label.is_empty() {
            s.outcome.clone()
        } else {
            label.to_string()
        }
    } else {
        "tamamlanmadı (görev sürüyor ya da süreç kapandı)".to_string()
    };
    let prompt = s.prompt_tokens.map_or_else(|| "—".to_string(), commas);
    let cache = s
        .cache_pct
        .map_or_else(|| "—".to_string(), |p| format!("%{p}"));
    format!(
        "Özet: {} istek • prompt {prompt} token • önbellek {cache} • {} tekrarlanan çağrı • \
         {} engellenen çağrı • toplam {seconds:.1} sn • sonuç: {outcome}",
        s.requests, s.repeated, s.blocked
    )
}

pub fn header_line(s: &TaskSummary) -> String {
    let mut parts = vec![format!("Görev {}", s.task_id)];
    for v in [&s.ts, &s.model] {
        if !v.is_empty() {
            parts.push(v.clone());
        }
    }
    if !s.mode.is_empty() {
        parts.push(format!("kip: {}", s.mode));
    }
    parts.join(" • ")
}

/// `/debug` sonucu: uyarı, bilgi ya da düz metin tablo satırları.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugReport {
    Warning(String),
    Info(String),
    Lines(Vec<String>),
}

/// `print_last_task`: sondan `task_index`. görevin adım tablosu (düz metin biçimi).
pub fn last_task_report(
    workspace_root: &Path,
    task_index: usize,
    trace_enabled: bool,
) -> DebugReport {
    if task_index < 1 {
        return DebugReport::Warning(
            "Görev sırası 1 veya daha büyük olmalı (1 = son görev).".into(),
        );
    }
    let tasks = read_tasks(workspace_root, task_index);
    if tasks.is_empty() {
        let hint = if trace_enabled {
            ""
        } else {
            " İz yazımı EVREN_TRACE=false ile kapalı."
        };
        return DebugReport::Info(format!(
            "Henüz görev izi yok (.evren/logs/trace.jsonl bulunamadı veya boş). \
             Bir ajan görevi çalıştırınca adımlar burada görünür.{hint}"
        ));
    }
    if task_index > tasks.len() {
        return DebugReport::Warning(format!(
            "Yalnızca {} görev izi var; sondan {task_index}. görev bulunamadı.",
            tasks.len()
        ));
    }
    let records = &tasks[tasks.len() - task_index];
    let summary = summarize_task(records);
    let mut lines = vec![header_line(&summary)];
    if !summary.text_preview.is_empty() {
        lines.push(format!("İstek: {}", summary.text_preview));
    }
    lines.push("Adım\tTür\tÖzet\tms\tSonuç kr.\tDurum".into());
    for row in table_rows(records) {
        lines.push(row.join("\t"));
    }
    lines.push(summary_line(&summary));
    DebugReport::Lines(lines)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_trace(root: &Path, lines: &[Value]) {
        let path = trace_path(root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(path, format!("{body}bozuk satır\n{{\"x\":1}}\n")).unwrap();
    }

    #[test]
    fn empty_and_bad_index() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            last_task_report(dir.path(), 0, true),
            DebugReport::Warning(_)
        ));
        let DebugReport::Info(msg) = last_task_report(dir.path(), 1, false) else {
            panic!("bilgi beklenirdi");
        };
        assert!(msg.ends_with("İz yazımı EVREN_TRACE=false ile kapalı."));
    }

    #[test]
    fn table_and_summary() {
        let dir = tempfile::tempdir().unwrap();
        write_trace(
            dir.path(),
            &[
                json!({"task_id":"a","type":"task_start","ts":"T","model":"m","mode":"normal","text_preview":"merhaba"}),
                json!({"task_id":"a","type":"request","step":1,"model":"m","prompt_tokens":2000,"cached_tokens":1000,"completion_tokens":50,"ms":300,"finish_reason":"tool_calls"}),
                json!({"task_id":"a","type":"tool","step":1,"tool":"read_file","args":"path=a","sig":"s1","ms":5,"result_chars":1234,"ok":true}),
                json!({"task_id":"a","type":"tool","step":2,"tool":"read_file","args":"path=a","sig":"s1","ms":5,"result_chars":10,"ok":false,"blocked":true}),
                json!({"task_id":"a","type":"task_end","ms":2500,"outcome":"ok"}),
                json!({"task_id":"b","type":"task_start","ts":"T2","model":"m","mode":"ask"}),
                json!({"task_id":"b","type":"request","step":1,"model":"x","ms":100}),
            ],
        );
        let DebugReport::Lines(lines) = last_task_report(dir.path(), 2, true) else {
            panic!("tablo beklenirdi");
        };
        assert_eq!(lines[0], "Görev a • T • m • kip: normal");
        assert_eq!(lines[1], "İstek: merhaba");
        assert_eq!(lines[2], "Adım\tTür\tÖzet\tms\tSonuç kr.\tDurum");
        assert_eq!(
            lines[3],
            "1\tistek\tprompt 2,000 (önbellek %50) • çıktı 50 • tool_calls\t300\t\t"
        );
        assert_eq!(lines[4], "1\tread_file\tpath=a\t5\t1,234\t");
        assert_eq!(lines[5], "2\tread_file\tpath=a\t5\t10\tENGELLENDİ");
        assert_eq!(
            lines[6],
            "Özet: 1 istek • prompt 2,000 token • önbellek %50 • 1 tekrarlanan çağrı • \
             1 engellenen çağrı • toplam 2.5 sn • sonuç: tamam"
        );
        let DebugReport::Lines(last) = last_task_report(dir.path(), 1, true) else {
            panic!("tablo beklenirdi");
        };
        assert_eq!(last[1], "Adım\tTür\tÖzet\tms\tSonuç kr.\tDurum");
        assert_eq!(last[2], "1\tistek\tx • kullanım bilgisi yok\t100\t\t");
        assert!(last[3].contains("prompt — token • önbellek —"));
        assert!(last[3]
            .ends_with("toplam 0.1 sn • sonuç: tamamlanmadı (görev sürüyor ya da süreç kapandı)"));
        assert!(matches!(
            last_task_report(dir.path(), 3, true),
            DebugReport::Warning(_)
        ));
    }
}
