//! Geçmiş sıkıştırma (Python `history_compactor.py` + `summarizer.py`).
//!
//! Aşamalar: araç çıktılarını özet satırına indir → orta kısmı özetle →
//! araç çıktılarını kırp → ortayı at → yalnız sistem + son birim.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::{json, Value};

use crate::token_manager::TokenManager;

pub const TOOL_HEAD_CHARS: usize = 1500;
pub const TOOL_TAIL_CHARS: usize = 500;
pub const TOOL_TRUNCATION_MARKER: &str = "\n... [araç çıktısı bağlam için kırpıldı] ...\n";
pub const STUB_PREFIX: &str = "[özet] ";
pub const SUMMARY_PREFIX: &str = "[Önceki adımların özeti]";
pub const STUB_MIN_CHARS: usize = 240;
pub const KEEP_RAW_UNITS_STUB: usize = 2;
pub const KEEP_RAW_UNITS_SUMMARY: usize = 4;
pub const FALLBACK_SUMMARY_CHARS: usize = 3500;

pub const SUMMARY_MAX_TOKENS: u32 = 600;
pub const SUMMARY_MAX_CHARS: usize = 1200;
pub const SUMMARY_TIMEOUT_SECONDS: f64 = 20.0;
pub const TRANSCRIPT_MAX_CHARS: usize = 14000;
const PART_CHARS: usize = 700;

pub const SUMMARY_INSTRUCTION: &str = "Aşağıda bir yazılım ajanının önceki adımlarının dökümü var. Devam eden iş için kısa bir özet yaz (en fazla 1200 karakter, Türkçe, madde işaretli). Şunları koru: dosya yolları, alınan kararlar, görülen hatalar ve çözümleri, kalan işler. Gereksiz ayrıntıyı, araç çıktılarının içeriğini ve selamlaşmayı yazma. Yalnızca özeti döndür.";

fn key_line_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(\d+\s+(failed|passed|error)|FAIL|Error|error|HATA)")
            .unwrap_or_else(|_| Regex::new("HATA").unwrap_or_else(|_| unreachable_regex()))
    })
}

fn range_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Satırlar (\d+)-(\d+)").unwrap_or_else(|_| unreachable_regex()))
}

/// Sabit desenler derlenemezse (olmaz) boş deseni döndürür.
fn unreachable_regex() -> Regex {
    #[allow(clippy::unwrap_used)]
    Regex::new("").unwrap()
}

// ---------- metin yardımcıları ----------

/// Python `text[:n]` (karakter bazlı).
pub fn head_chars(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// Python `text[-n:]` (karakter bazlı).
pub fn tail_chars(text: &str, n: usize) -> &str {
    let count = text.chars().count();
    if count <= n {
        return text;
    }
    let skip = count - n;
    match text.char_indices().nth(skip) {
        Some((i, _)) => &text[i..],
        None => text,
    }
}

/// `f"{n:,}".replace(",", ".")`.
pub fn kr(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && i % 3 == len % 3 {
            out.push('.');
        }
        out.push(ch);
    }
    out
}

/// Boşlukları tek boşluğa indirir; `limit` aşılırsa `…` ile kısaltır.
pub fn clip(text: &str, limit: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > limit {
        let mut s = head_chars(&joined, limit.saturating_sub(1)).to_string();
        s.push('…');
        s
    } else {
        joined
    }
}

/// İlk boş olmayan satır (kırpılmış).
pub fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

pub fn truncate_tool_content(text: &str) -> String {
    if text.chars().count() <= TOOL_HEAD_CHARS + TOOL_TAIL_CHARS {
        return text.to_string();
    }
    format!(
        "{}{TOOL_TRUNCATION_MARKER}{}",
        head_chars(text, TOOL_HEAD_CHARS),
        tail_chars(text, TOOL_TAIL_CHARS)
    )
}

fn content_str(m: &Value) -> Option<&str> {
    m.get("content").and_then(Value::as_str)
}

fn role(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or("")
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Araç sonucunun tek satırlık özeti (`[özet] ...`).
pub fn build_stub(name: &str, args: &Value, result: &str) -> String {
    let size = format!("({} kr)", kr(result.chars().count()));
    let head = first_line(result);
    match name {
        "view_file" => {
            let path = arg_str(args, "path").unwrap_or("");
            if result.starts_with("HATA") {
                return format!(
                    "{STUB_PREFIX}view_file {} → {} {size}",
                    if path.is_empty() { "?" } else { path },
                    clip(&head, 100)
                );
            }
            let rng = range_re()
                .captures(head_chars(result, 300))
                .map(|c| format!(" {}-{}", &c[1], &c[2]))
                .unwrap_or_default();
            format!("{STUB_PREFIX}view_file {path}{rng} okundu {size}")
        }
        "run_command" | "ssh_run" => {
            let cmd = clip(arg_str(args, "command").unwrap_or(""), 60);
            let outcome = if head.starts_with("Çıkış Kodu") {
                let mut outcome = head.replace("Çıkış Kodu:", "Çıkış Kodu");
                let lines: Vec<&str> = result
                    .lines()
                    .skip(1)
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
                let mut key = lines
                    .iter()
                    .rev()
                    .find(|l| key_line_re().is_match(l))
                    .copied();
                if key.is_none() && !outcome.ends_with(" 0") {
                    key = lines.last().copied();
                }
                if let Some(k) = key {
                    outcome.push_str(", ");
                    outcome.push_str(&clip(k, 80));
                }
                outcome
            } else {
                clip(&head, 100)
            };
            format!("{STUB_PREFIX}{name} \"{cmd}\" → {outcome} {size}")
        }
        _ => {
            let shown: Vec<String> = args
                .as_object()
                .map(|o| {
                    o.values()
                        .filter_map(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .take(2)
                        .map(|s| clip(s, 50))
                        .collect()
                })
                .unwrap_or_default();
            let arg_text = if shown.is_empty() {
                String::new()
            } else {
                format!(" {}", shown.join(" "))
            };
            format!(
                "{STUB_PREFIX}{name}{arg_text} → {} {size}",
                clip(&head, 100)
            )
        }
    }
}

/// tool_call id → (araç adı, argümanlar).
fn call_index(messages: &[Value]) -> HashMap<String, (String, Value)> {
    let mut index = HashMap::new();
    for m in messages {
        let Some(calls) = m.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let func = call.get("function");
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let args = match func.and_then(|f| f.get("arguments")) {
                Some(Value::String(s)) => serde_json::from_str::<Value>(s)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({})),
                Some(v @ Value::Object(_)) => v.clone(),
                _ => json!({}),
            };
            index.insert(id, (name, args));
        }
    }
    index
}

/// Gövdeyi `[user*] assistant [tool*]` birimlerine böler.
pub fn group_units(body: &[Value]) -> Vec<Vec<Value>> {
    let mut units: Vec<Vec<Value>> = Vec::new();
    let mut current: Vec<Value> = Vec::new();
    let mut has_assistant = false;
    for m in body {
        let r = role(m);
        if !current.is_empty() {
            let last = current.last().map(role).unwrap_or("");
            if (r == "user" && last != "user") || (r == "assistant" && has_assistant) {
                units.push(std::mem::take(&mut current));
                has_assistant = false;
            }
        }
        if r == "assistant" {
            has_assistant = true;
        }
        current.push(m.clone());
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

fn is_task_start(m: &Value) -> bool {
    role(m) == "user"
        && content_str(m).is_some_and(|c| !c.starts_with("[EVREN]") && !c.starts_with("[Önceki"))
}

/// Mevcut görevde her (yol, aralık) için ilk okunan view_file sonucu (tool_call_id).
fn current_task_reads(
    messages: &[Value],
    calls: &HashMap<String, (String, Value)>,
) -> HashSet<String> {
    let task_start = messages.iter().rposition(is_task_start).unwrap_or(0);
    let mut seen = HashSet::new();
    let mut keep = HashSet::new();
    for m in messages[task_start..].iter().rev() {
        if role(m) != "tool" {
            continue;
        }
        let id = m.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
        let Some((name, args)) = calls.get(id) else {
            continue;
        };
        if name != "view_file" {
            continue;
        }
        let key = format!(
            "{}:{}:{}",
            args.get("path").map(json_text).unwrap_or_default(),
            args.get("start_line").map(json_text).unwrap_or_default(),
            args.get("end_line").map(json_text).unwrap_or_default()
        );
        if seen.insert(key) {
            keep.insert(id.to_string());
        }
    }
    keep
}

fn json_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// Korunan birimler dışındaki uzun araç çıktılarını özet satırına indirir.
pub fn stub_tool_results(
    messages: &[Value],
    keep_units: usize,
    keep_current_reads: bool,
) -> Vec<Value> {
    let start = usize::from(messages.first().is_some_and(|m| role(m) == "system"));
    let units = group_units(&messages[start..]);
    let mut protected: HashSet<String> = HashSet::new();
    for unit in units.iter().rev().take(keep_units) {
        for m in unit {
            if let Some(id) = m.get("tool_call_id").and_then(Value::as_str) {
                protected.insert(id.to_string());
            }
        }
    }
    let calls = call_index(messages);
    if keep_current_reads {
        protected.extend(current_task_reads(messages, &calls));
    }
    let mut out = messages.to_vec();
    for m in out.iter_mut() {
        if role(m) != "tool" {
            continue;
        }
        let Some(content) = content_str(m).map(str::to_string) else {
            continue;
        };
        let id = m
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if protected.contains(&id)
            || content.starts_with(STUB_PREFIX)
            || content.chars().count() <= STUB_MIN_CHARS
        {
            continue;
        }
        let (name, args) = calls
            .get(&id)
            .cloned()
            .unwrap_or_else(|| (String::new(), json!({})));
        let name = if name.is_empty() {
            m.get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("araç")
                .to_string()
        } else {
            name
        };
        let stub = build_stub(&name, &args, &content);
        if let Some(obj) = m.as_object_mut() {
            obj.insert("content".into(), Value::String(stub));
        }
    }
    out
}

fn stub_text(
    messages: &[Value],
    calls: &HashMap<String, (String, Value)>,
    m: &Value,
) -> Option<String> {
    let _ = messages;
    let content = content_str(m)?;
    let id = m.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
    let (name, args) = calls
        .get(id)
        .cloned()
        .unwrap_or_else(|| (String::new(), json!({})));
    let name = if name.is_empty() {
        m.get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("araç")
            .to_string()
    } else {
        name
    };
    let stub = if content.starts_with(STUB_PREFIX) {
        content.to_string()
    } else {
        build_stub(&name, &args, content)
    };
    Some(stub.strip_prefix(STUB_PREFIX).unwrap_or(&stub).to_string())
}

/// (görev metni, son cevap, araç özetleri).
fn stub_lines(messages: &[Value]) -> (String, String, Vec<String>) {
    let calls = call_index(messages);
    let mut task = String::new();
    let mut answer = String::new();
    let mut stubs = Vec::new();
    for m in messages {
        match role(m) {
            "user" => {
                if task.is_empty() {
                    task = content_str(m).unwrap_or("").to_string();
                }
            }
            "assistant" => {
                let has_calls = m
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|c| !c.is_empty());
                let text = content_str(m).unwrap_or("").trim();
                if !has_calls && !text.is_empty() {
                    answer = text.to_string();
                }
            }
            "tool" => {
                if let Some(s) = stub_text(messages, &calls, m) {
                    stubs.push(s);
                }
            }
            _ => {}
        }
    }
    (task, answer, stubs)
}

/// Model olmadan üretilen özet.
pub fn deterministic_summary(messages: &[Value]) -> String {
    let calls = call_index(messages);
    let mut lines = Vec::new();
    for m in messages {
        match role(m) {
            "user" => {
                let content = content_str(m).unwrap_or("");
                if let Some(rest) = content.strip_prefix(SUMMARY_PREFIX) {
                    lines.push(clip(rest, 1200));
                } else {
                    lines.push(format!("- Görev: {}", clip(content, 200)));
                }
            }
            "assistant" => {
                let text = content_str(m).unwrap_or("").trim();
                if !text.is_empty() {
                    lines.push(format!("- Ajan: {}", clip(text, 200)));
                }
            }
            "tool" => {
                if let Some(s) = stub_text(messages, &calls, m) {
                    lines.push(format!("- {s}"));
                }
            }
            _ => {}
        }
    }
    let text = lines.join("\n");
    if text.chars().count() > FALLBACK_SUMMARY_CHARS {
        format!("…\n{}", tail_chars(&text, FALLBACK_SUMMARY_CHARS))
    } else {
        text
    }
}

/// Biten görevi tek kullanıcı mesajına indirger.
pub fn collapse_task(messages: &[Value]) -> String {
    let (task, answer, stubs) = stub_lines(messages);
    let mut parts = vec![format!("[Önceki görev özeti] Görev: {}", clip(&task, 300))];
    if !answer.is_empty() {
        parts.push(format!("Sonuç: {}", clip(&answer, 600)));
    }
    if !stubs.is_empty() {
        let start = stubs.len().saturating_sub(25);
        let lines: Vec<String> = stubs[start..]
            .iter()
            .map(|s| format!("- {}", clip(s, 160)))
            .collect();
        parts.push(format!("Adımlar:\n{}", lines.join("\n")));
    }
    parts.join("\n")
}

// ---------- özetleyici (summarizer.py) ----------

/// Model özetleyici için düz döküm.
pub fn render_transcript(messages: &[Value]) -> String {
    let mut lines = Vec::new();
    for m in messages {
        let text = content_str(m).unwrap_or("").trim();
        match role(m) {
            "assistant" => {
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let func = call.get("function");
                        let name = func
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let args = match func.and_then(|f| f.get("arguments")) {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Null) | None => String::new(),
                            Some(v) => v.to_string(),
                        };
                        lines.push(format!("ajan araç: {name} {}", clip(&args, 160)));
                    }
                }
                if !text.is_empty() {
                    lines.push(format!("ajan: {}", clip(text, PART_CHARS)));
                }
            }
            "tool" => lines.push(format!("sonuç: {}", clip(text, 260))),
            "user" => lines.push(format!("kullanıcı: {}", clip(text, PART_CHARS))),
            _ => {}
        }
    }
    let out = lines.join("\n");
    if out.chars().count() > TRANSCRIPT_MAX_CHARS {
        format!("…\n{}", tail_chars(&out, TRANSCRIPT_MAX_CHARS))
    } else {
        out
    }
}

/// Model özetini sınıra kırpar.
pub fn clip_summary(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() > SUMMARY_MAX_CHARS {
        format!("{}…", head_chars(text, SUMMARY_MAX_CHARS - 1).trim_end())
    } else {
        text.to_string()
    }
}

/// Özetleme isteği gövdesi (`ChatRequest` için mesajlar).
pub fn summary_request_messages(messages: &[Value]) -> Vec<Value> {
    vec![
        json!({"role": "system", "content": SUMMARY_INSTRUCTION}),
        json!({"role": "user", "content": render_transcript(messages)}),
    ]
}

// ---------- HistoryCompactor ----------

/// Eski birimleri modelle özetleyen geri çağrı; `None` → deterministik özet.
pub type Summarizer = Box<dyn Fn(&[Value]) -> Option<String> + Send + Sync>;

pub struct HistoryCompactor {
    pub smart: bool,
    pub summarizer: Option<Summarizer>,
    pub target_ratio: f64,
    /// Akıllı sıkıştırma olayları.
    pub events: u32,
}

impl HistoryCompactor {
    pub fn new(smart: bool, target_ratio: f64) -> Self {
        HistoryCompactor {
            smart,
            summarizer: None,
            target_ratio,
            events: 0,
        }
    }

    pub fn with_summarizer(mut self, summarizer: Summarizer) -> Self {
        self.summarizer = Some(summarizer);
        self
    }

    fn fits(tm: &TokenManager, messages: &[Value], output_budget: i64, limit: i64) -> bool {
        tm.estimate_messages(messages) + output_budget <= limit
    }

    /// Mesajları `limit` (yoksa bağlam penceresi) altına indirir.
    pub fn compact(
        &mut self,
        tm: &TokenManager,
        messages: &[Value],
        output_budget: i64,
        max_tokens: Option<i64>,
    ) -> Vec<Value> {
        if messages.is_empty() {
            return Vec::new();
        }
        let working = messages.to_vec();
        let limit = max_tokens.unwrap_or(i64::from(tm.context_window));
        if Self::fits(tm, &working, output_budget, limit) {
            return working;
        }
        if self.smart {
            self.events += 1;
            let target = (limit as f64 * self.target_ratio) as i64;
            let stubbed = stub_tool_results(&working, KEEP_RAW_UNITS_STUB, true);
            if Self::fits(tm, &stubbed, output_budget, limit) {
                return stubbed;
            }
            let stubbed = stub_tool_results(&working, KEEP_RAW_UNITS_STUB, false);
            if Self::fits(tm, &stubbed, output_budget, target) {
                return stubbed;
            }
            let summarized = self.stage_summarize_middle(&stubbed);
            if Self::fits(tm, &summarized, output_budget, limit) {
                return summarized;
            }
            let working = summarized;
            let truncated = Self::stage_truncate_tools(&working);
            if Self::fits(tm, &truncated, output_budget, limit) {
                return truncated;
            }
            let dropped = Self::stage_drop_middle(tm, &truncated, output_budget, limit);
            if Self::fits(tm, &dropped, output_budget, limit) {
                return dropped;
            }
            return Self::stage_system_and_last(&dropped);
        }
        let truncated = Self::stage_truncate_tools(&working);
        if Self::fits(tm, &truncated, output_budget, limit) {
            return truncated;
        }
        let dropped = Self::stage_drop_middle(tm, &truncated, output_budget, limit);
        if Self::fits(tm, &dropped, output_budget, limit) {
            return dropped;
        }
        Self::stage_system_and_last(&dropped)
    }

    fn stage_summarize_middle(&self, messages: &[Value]) -> Vec<Value> {
        let (system, body): (Vec<Value>, Vec<Value>) =
            messages.iter().cloned().partition(|m| role(m) == "system");
        let Some(first_user) = body.iter().position(|m| role(m) == "user") else {
            return messages.to_vec();
        };
        let head = &body[..=first_user];
        let rest = &body[first_user + 1..];
        let units = group_units(rest);
        if units.len() <= KEEP_RAW_UNITS_SUMMARY {
            return messages.to_vec();
        }
        let split = units.len() - KEEP_RAW_UNITS_SUMMARY;
        let old: Vec<Value> = units[..split].iter().flatten().cloned().collect();
        let recent: Vec<Value> = units[split..].iter().flatten().cloned().collect();
        let text = self
            .summarizer
            .as_ref()
            .and_then(|s| s(&old))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| deterministic_summary(&old));
        let mut out = system;
        out.extend(head.iter().cloned());
        out.push(json!({"role": "user", "content": format!("{SUMMARY_PREFIX}\n{text}")}));
        out.extend(recent);
        out
    }

    fn stage_truncate_tools(messages: &[Value]) -> Vec<Value> {
        let mut out = messages.to_vec();
        for m in out.iter_mut() {
            if role(m) != "tool" {
                continue;
            }
            if let Some(content) = content_str(m).map(truncate_tool_content) {
                if let Some(obj) = m.as_object_mut() {
                    obj.insert("content".into(), Value::String(content));
                }
            }
        }
        out
    }

    fn stage_drop_middle(
        tm: &TokenManager,
        messages: &[Value],
        output_budget: i64,
        limit: i64,
    ) -> Vec<Value> {
        let (system, body): (Vec<Value>, Vec<Value>) =
            messages.iter().cloned().partition(|m| role(m) == "system");
        let mut units: Vec<Vec<Value>> = Vec::new();
        for m in body {
            if role(&m) == "assistant" || units.is_empty() {
                units.push(vec![m]);
            } else if let Some(last) = units.last_mut() {
                last.push(m);
            }
        }
        let assemble = |units: &[Vec<Value>]| -> Vec<Value> {
            let mut out = system.clone();
            out.extend(units.iter().flatten().cloned());
            out
        };
        while units.len() > 1 {
            let candidate = assemble(&units);
            if Self::fits(tm, &candidate, output_budget, limit) {
                return candidate;
            }
            units.remove(0);
        }
        assemble(&units)
    }

    fn stage_system_and_last(messages: &[Value]) -> Vec<Value> {
        let mut out: Vec<Value> = messages
            .iter()
            .filter(|m| role(m) == "system")
            .cloned()
            .collect();
        let body: Vec<&Value> = messages.iter().filter(|m| role(m) != "system").collect();
        if body.is_empty() {
            return out;
        }
        let mut start = body.len() - 1;
        if role(body[start]) == "tool" {
            while start > 0 && role(body[start]) != "assistant" {
                start -= 1;
            }
        }
        out.extend(body[start..].iter().map(|m| (*m).clone()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(kr(1234567), "1.234.567");
        assert_eq!(kr(12), "12");
        assert_eq!(clip("a   b\n c", 100), "a b c");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(first_line("\n\n  x \ny"), "x");
        let long = "a".repeat(2001);
        assert!(truncate_tool_content(&long).contains(TOOL_TRUNCATION_MARKER));
        assert_eq!(truncate_tool_content("kısa"), "kısa");
    }

    #[test]
    fn stubs() {
        let s = build_stub(
            "view_file",
            &json!({"path": "a.py"}),
            "Satırlar 1-20 / 100\nkod",
        );
        assert_eq!(s, "[özet] view_file a.py 1-20 okundu (23 kr)");
        let s = build_stub("view_file", &json!({}), "HATA: yok");
        assert_eq!(s, "[özet] view_file ? → HATA: yok (9 kr)");
        let s = build_stub(
            "run_command",
            &json!({"command": "pytest"}),
            "Çıkış Kodu: 1\n3 failed\nbye",
        );
        assert_eq!(
            s,
            "[özet] run_command \"pytest\" → Çıkış Kodu 1, 3 failed (26 kr)"
        );
        let s = build_stub(
            "run_command",
            &json!({"command": "ls"}),
            "Çıkış Kodu: 0\na\nb",
        );
        assert_eq!(s, "[özet] run_command \"ls\" → Çıkış Kodu 0 (17 kr)");
        let s = build_stub(
            "find_files",
            &json!({"query": "x", "n": 3, "path": "src"}),
            "sonuç",
        );
        assert_eq!(s, "[özet] find_files x src → sonuç (5 kr)");
    }

    fn msgs() -> Vec<Value> {
        let big = "x".repeat(300);
        vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "görev"}),
            json!({"role": "assistant", "content": "", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "view_file", "arguments": "{\"path\":\"a\"}"}}]}),
            json!({"role": "tool", "tool_call_id": "c1", "name": "view_file", "content": big}),
            json!({"role": "assistant", "content": "", "tool_calls": [
                {"id": "c2", "type": "function", "function": {"name": "view_file", "arguments": "{\"path\":\"b\"}"}}]}),
            json!({"role": "tool", "tool_call_id": "c2", "name": "view_file", "content": big}),
            json!({"role": "assistant", "content": "", "tool_calls": [
                {"id": "c3", "type": "function", "function": {"name": "view_file", "arguments": "{\"path\":\"c\"}"}}]}),
            json!({"role": "tool", "tool_call_id": "c3", "name": "view_file", "content": big}),
            json!({"role": "assistant", "content": "bitti"}),
        ]
    }

    #[test]
    fn units_and_stubbing() {
        let m = msgs();
        assert_eq!(group_units(&m[1..]).len(), 4);
        let out = stub_tool_results(&m, 2, false);
        assert!(out[3]["content"]
            .as_str()
            .unwrap_or("")
            .starts_with("[özet] view_file a okundu"));
        assert!(out[5]["content"]
            .as_str()
            .unwrap_or("")
            .starts_with("[özet]"));
        assert_eq!(out[7]["content"], m[7]["content"]);
        let out = stub_tool_results(&m, 0, true);
        // Her yol ilk kez okunuyor: hepsi korunur.
        assert_eq!(out[3]["content"], m[3]["content"]);
    }

    #[test]
    fn compaction_stages() {
        let tm = TokenManager::new(100);
        let mut c = HistoryCompactor::new(true, 0.5);
        let out = c.compact(&tm, &msgs(), 10, None);
        assert_eq!(out[0]["role"], "system");
        assert!(out.len() < msgs().len());
        assert_eq!(c.events, 1);
        let summary = deterministic_summary(&msgs()[1..]);
        assert!(summary.starts_with("- Görev: görev"));
        let collapsed = collapse_task(&msgs()[1..]);
        assert!(collapsed.contains("Sonuç: bitti"));
        assert!(collapsed.contains("Adımlar:\n- view_file a okundu"));
    }

    #[test]
    fn transcript_and_summary_clip() {
        let t = render_transcript(&msgs());
        assert!(t.starts_with("kullanıcı: görev\najan araç: view_file {\"path\":\"a\"}"));
        let long = "y".repeat(1300);
        assert_eq!(clip_summary(&long).chars().count(), 1200);
    }
}
