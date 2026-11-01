//! Sunucu gönderimli olaylar (SSE) ayrıştırıcısı ve OpenAI uyumlu akış
//! parçalarını tek bir yanıtta birleştiren toplayıcı (`chat_stream`).

use evren_core::EvrenError;
use serde_json::{json, Map, Value};

/// Akıştan tüketiciye iletilen artımlı parça.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamDelta {
    /// `reasoning` / `reasoning_content` akışı.
    Reasoning(String),
    /// Görünür içerik akışı.
    Content(String),
    /// Birleştirilmiş araç çağrıları (her parça sonrası güncel durum).
    ToolCalls(Vec<Value>),
}

/// Satır tabanlı SSE ayrıştırıcı: `data:` alanlarını toplar, `:` ile
/// başlayan yorum satırlarını (`: keep-alive`) atlar, boş satırda olayı
/// teslim eder. `[DONE]` görülünce `done` bayrağı kalkar.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    data: Vec<String>,
    pub done: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Yeni ham baytları alır; tamamlanan olayların `data` gövdelerini döner.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        let mut events = Vec::new();
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(ev) = self.feed_line(line) {
                events.push(ev);
            }
        }
        events
    }

    /// Bağlantı kapanınca tamponda kalan son olayı teslim eder.
    pub fn finish(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            if let Some(ev) = self.feed_line(line.trim_end_matches(['\n', '\r'])) {
                events.push(ev);
            }
        }
        if let Some(ev) = self.dispatch() {
            events.push(ev);
        }
        events
    }

    fn feed_line(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        if field == "data" {
            self.data.push(value.to_owned());
        }
        None
    }

    fn dispatch(&mut self) -> Option<String> {
        if self.data.is_empty() {
            return None;
        }
        let payload = std::mem::take(&mut self.data).join("\n");
        if payload.trim() == "[DONE]" {
            self.done = true;
            return None;
        }
        Some(payload)
    }
}

/// `chat_stream` toplayıcısı: parçaları `content`, `reasoning`, `tool_calls`
/// (indeks bazlı birleştirme), `finish_reason` ve `usage` alanlarında biriktirir.
#[derive(Debug, Default)]
pub struct StreamAssembler {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<Value>,
    pub finish_reason: Option<String>,
    pub usage: Option<Value>,
    pub model: Option<String>,
}

impl StreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bir `data:` gövdesini işler; üretilen artımlı parçaları döner.
    /// Üst düzey `error` alanı API hatası olarak döner.
    pub fn apply(&mut self, payload: &str) -> Result<Vec<StreamDelta>, EvrenError> {
        let chunk: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };
        if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| err.to_string());
            return Err(EvrenError::Api {
                message,
                status_code: None,
                failed_key: None,
            });
        }
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(u.clone());
        }
        if self.model.is_none() {
            if let Some(m) = chunk.get("model").and_then(Value::as_str) {
                self.model = Some(m.to_owned());
            }
        }
        let mut out = Vec::new();
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return Ok(out);
        };
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(fr.to_owned());
        }
        let Some(delta) = choice.get("delta").and_then(Value::as_object) else {
            return Ok(out);
        };
        let reasoning = delta
            .get("reasoning")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                delta
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            });
        if let Some(r) = reasoning {
            self.reasoning.push_str(r);
            out.push(StreamDelta::Reasoning(r.to_owned()));
        }
        if let Some(c) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            self.content.push_str(c);
            out.push(StreamDelta::Content(c.to_owned()));
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
            if !tcs.is_empty() {
                for tc in tcs {
                    self.merge_tool_call(tc);
                }
                out.push(StreamDelta::ToolCalls(self.tool_calls.clone()));
            }
        }
        Ok(out)
    }

    fn merge_tool_call(&mut self, tc: &Value) {
        let idx = tc
            .get("index")
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .unwrap_or(self.tool_calls.len());
        while self.tool_calls.len() <= idx {
            self.tool_calls.push(json!({
                "id": "",
                "type": "function",
                "function": {"name": "", "arguments": ""}
            }));
        }
        let slot = &mut self.tool_calls[idx];
        if let Some(id) = tc
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            slot["id"] = Value::from(id);
        }
        if let Some(t) = tc
            .get("type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            slot["type"] = Value::from(t);
        }
        if let Some(f) = tc.get("function").and_then(Value::as_object) {
            if let Some(n) = f.get("name").and_then(Value::as_str) {
                append_str(&mut slot["function"]["name"], n);
            }
            if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                append_str(&mut slot["function"]["arguments"], a);
            }
        }
    }

    pub fn streamed_any(&self) -> bool {
        !self.content.is_empty() || !self.reasoning.is_empty() || !self.tool_calls.is_empty()
    }

    /// Akışsız `chat/completions` yanıtıyla aynı biçimde ham JSON üretir.
    pub fn into_raw(self) -> Value {
        let mut message = Map::new();
        message.insert("role".into(), Value::from("assistant"));
        message.insert("content".into(), Value::from(self.content));
        if !self.reasoning.is_empty() {
            message.insert("reasoning".into(), Value::from(self.reasoning));
        }
        if !self.tool_calls.is_empty() {
            message.insert("tool_calls".into(), Value::Array(self.tool_calls));
        }
        let mut raw = Map::new();
        if let Some(m) = self.model {
            raw.insert("model".into(), Value::from(m));
        }
        raw.insert(
            "choices".into(),
            json!([{
                "index": 0,
                "message": Value::Object(message),
                "finish_reason": self.finish_reason,
            }]),
        );
        raw.insert("usage".into(), self.usage.unwrap_or(Value::Null));
        Value::Object(raw)
    }
}

fn append_str(slot: &mut Value, extra: &str) {
    match slot {
        Value::String(s) => s.push_str(extra),
        _ => *slot = Value::from(extra),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parser_skips_comments_and_joins_data() {
        let mut p = SseParser::new();
        let ev = p.feed(b": keep-alive\r\n\r\ndata: {\"a\":1}\n\ndata: x\ndata: y\n\n");
        assert_eq!(ev, vec!["{\"a\":1}".to_owned(), "x\ny".to_owned()]);
        assert!(!p.done);
        let ev = p.feed(b"data: [DONE]\n\n");
        assert!(ev.is_empty());
        assert!(p.done);
    }

    #[test]
    fn parser_handles_split_chunks_and_finish() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: {\"pa").is_empty());
        assert_eq!(p.feed(b"rt\":1}\n\n"), vec!["{\"part\":1}".to_owned()]);
        assert!(p.feed(b"data: tail").is_empty());
        assert_eq!(p.finish(), vec!["tail".to_owned()]);
    }

    #[test]
    fn assembler_merges_content_reasoning_tool_calls() {
        let mut a = StreamAssembler::new();
        let d1 = a
            .apply(r#"{"choices":[{"delta":{"reasoning":"think "}}]}"#)
            .unwrap();
        assert_eq!(d1, vec![StreamDelta::Reasoning("think ".into())]);
        a.apply(r#"{"choices":[{"delta":{"reasoning_content":"more"}}]}"#)
            .unwrap();
        a.apply(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#)
            .unwrap();
        a.apply(r#"{"choices":[{"delta":{"content":"lo"}}]}"#)
            .unwrap();
        a.apply(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"view_","arguments":"{\"pa"}}]}}]}"#)
            .unwrap();
        let d = a
            .apply(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"file","arguments":"th\": \"a.py\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"total_tokens":15}}"#)
            .unwrap();
        assert!(matches!(d.last(), Some(StreamDelta::ToolCalls(t)) if t.len() == 1));
        assert!(a.streamed_any());
        let raw = a.into_raw();
        let msg = &raw["choices"][0]["message"];
        assert_eq!(msg["content"], "Hello");
        assert_eq!(msg["reasoning"], "think more");
        assert_eq!(msg["tool_calls"][0]["id"], "c1");
        assert_eq!(msg["tool_calls"][0]["function"]["name"], "view_file");
        assert_eq!(
            msg["tool_calls"][0]["function"]["arguments"],
            "{\"path\": \"a.py\"}"
        );
        assert_eq!(raw["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(raw["usage"]["total_tokens"], 15);
    }

    #[test]
    fn assembler_reports_error_payload() {
        let mut a = StreamAssembler::new();
        let e = a
            .apply(r#"{"error":{"message":"kota doldu"}}"#)
            .unwrap_err();
        assert!(matches!(e, EvrenError::Api { message, .. } if message == "kota doldu"));
        assert!(a.apply("bozuk json").unwrap().is_empty());
    }
}
