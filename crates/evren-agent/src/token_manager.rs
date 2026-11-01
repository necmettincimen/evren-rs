//! Token tahmini ve kalibrasyon (Python `token_manager.py`).

use evren_api::ChatResponse;
use evren_core::models::floor_output_tokens;
use serde_json::Value;

pub const DEFAULT_CHARS_PER_TOKEN: f64 = 3.5;

#[derive(Debug, Clone)]
pub struct TokenManager {
    pub chars_per_token: f64,
    pub context_window: u32,
    calibration: f64,
    calibration_samples: u32,
}

impl TokenManager {
    pub fn new(context_window: u32) -> Self {
        TokenManager {
            chars_per_token: DEFAULT_CHARS_PER_TOKEN,
            context_window,
            calibration: 1.0,
            calibration_samples: 0,
        }
    }

    pub fn calibration(&self) -> f64 {
        self.calibration
    }

    pub fn calibration_samples(&self) -> u32 {
        self.calibration_samples
    }

    pub fn estimate_text(&self, text: &str) -> i64 {
        if text.is_empty() {
            return 0;
        }
        let chars = text.chars().count() as f64;
        ((chars / self.chars_per_token) * self.calibration)
            .floor()
            .max(1.0) as i64
    }

    pub fn estimate_message(&self, message: &Value) -> i64 {
        let mut tokens = 0;
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            tokens += self.estimate_text(content);
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let func = call.get("function");
                let name = func
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let args_text = match func.and_then(|f| f.get("arguments")) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => other.to_string(),
                };
                tokens += self.estimate_text(name) + self.estimate_text(&args_text) + 4;
            }
        }
        tokens
    }

    pub fn estimate_messages(&self, messages: &[Value]) -> i64 {
        messages.iter().map(|m| self.estimate_message(m)).sum()
    }

    pub fn calibrate(&mut self, estimated: i64, actual: i64) {
        if estimated <= 0 || actual <= 0 {
            return;
        }
        let ratio = actual as f64 / estimated as f64;
        let alpha = 0.3;
        self.calibration = (1.0 - alpha) * self.calibration + alpha * ratio;
        self.calibration_samples += 1;
    }

    pub fn calibrate_from_response(&mut self, estimated: i64, response: &ChatResponse) {
        let prompt = response
            .usage()
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if prompt > 0 {
            self.calibrate(estimated, prompt);
        }
    }

    pub fn resolve_output_budget(&self, model: &str, requested: Option<u32>) -> u32 {
        floor_output_tokens(model, requested.unwrap_or(0))
    }

    pub fn remaining_context(&self, messages: &[Value], output_tokens: i64) -> i64 {
        i64::from(self.context_window) - self.estimate_messages(messages) - output_tokens
    }

    pub fn fits(&self, messages: &[Value], output_tokens: i64) -> bool {
        self.remaining_context(messages, output_tokens) > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn estimates() {
        let tm = TokenManager::new(1000);
        assert_eq!(tm.estimate_text(""), 0);
        assert_eq!(tm.estimate_text("ab"), 1);
        assert_eq!(tm.estimate_text("abcdefghij"), 2);
        let m = json!({"role": "assistant", "content": "abcdefg", "tool_calls": [
            {"function": {"name": "view_file", "arguments": "{\"path\": \"a\"}"}}
        ]});
        // "abcdefg" → 2, "view_file" → 2, 13 karakter argüman → 3, +4
        assert_eq!(tm.estimate_message(&m), 2 + 2 + 3 + 4);
    }

    #[test]
    fn calibration_moves() {
        let mut tm = TokenManager::new(1000);
        tm.calibrate(100, 200);
        assert!((tm.calibration() - 1.3).abs() < 1e-9);
        tm.calibrate(0, 5);
        assert!((tm.calibration() - 1.3).abs() < 1e-9);
        assert_eq!(tm.calibration_samples(), 1);
    }
}
