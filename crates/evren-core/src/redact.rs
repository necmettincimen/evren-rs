//! Kayıt öncesi sır maskeleme (Python `workflow_state.redact_text`). Kullanıcı
//! incelemesinin yerini tutmaz; yaygın kimlik bilgisi biçimlerini `[REDACTED]`
//! ile değiştirir.

use std::sync::OnceLock;

use regex::{Captures, Regex};

fn re(cell: &'static OnceLock<Option<Regex>>, pattern: &str) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern).ok()).as_ref()
}

/// Yaygın kimlik bilgisi biçimlerini maskeler.
pub fn redact_text(text: &str) -> String {
    static URL: OnceLock<Option<Regex>> = OnceLock::new();
    static AUTH: OnceLock<Option<Regex>> = OnceLock::new();
    static KV: OnceLock<Option<Regex>> = OnceLock::new();
    static LLM: OnceLock<Option<Regex>> = OnceLock::new();
    static PEM: OnceLock<Option<Regex>> = OnceLock::new();

    let mut out = text.to_string();
    if let Some(r) = re(&URL, r"(?i)([a-z][a-z0-9+.-]*://[^/\s:@]+:)[^@\s/]+(@)") {
        out = r.replace_all(&out, "${1}[REDACTED]${2}").into_owned();
    }
    // Önce kimlik şemaları: "Authorization: Bearer <tok>" yalnız "Bearer"
    // sözcüğünü maskeleyip belirteci açıkta bırakmasın.
    if let Some(r) = re(&AUTH, r#"(?i)\b((?:bearer|basic|digest)\s+)[^\s"',;&]+"#) {
        out = r.replace_all(&out, "${1}[REDACTED]").into_owned();
    }
    if let Some(r) = re(
        &KV,
        r#"(?i)(["']?(?:password|passwd|pwd|secret|token|api[_-]?key|authorization)["']?\s*[:=]\s*)((?:bearer|basic|digest)\s+\[REDACTED\]|"[^"]*"|'[^']*'|[^\s,;&"']+)"#,
    ) {
        out = r
            .replace_all(&out, |c: &Captures| {
                if c[2].ends_with("[REDACTED]") {
                    c[0].to_string()
                } else {
                    format!("{}[REDACTED]", &c[1])
                }
            })
            .into_owned();
    }
    if let Some(r) = re(&LLM, r"evren_llm_[A-Za-z0-9_-]+") {
        out = r.replace_all(&out, "[REDACTED]").into_owned();
    }
    if let Some(r) = re(
        &PEM,
        r"(?s)-----BEGIN [^-]*PRIVATE KEY-----.*?-----END [^-]*PRIVATE KEY-----",
    ) {
        out = r.replace_all(&out, "[REDACTED PRIVATE KEY]").into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_common_forms() {
        assert_eq!(
            redact_text("https://u:gizli@host/x"),
            "https://u:[REDACTED]@host/x"
        );
        assert_eq!(
            redact_text("Authorization: Bearer abc.def"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(redact_text("password=hunter2 x"), "password=[REDACTED] x");
        assert_eq!(redact_text("api_key: \"k1\""), "api_key: [REDACTED]");
        assert_eq!(
            redact_text("evren_llm_GIZLI123 ile giriş"),
            "[REDACTED] ile giriş"
        );
        assert_eq!(
            redact_text("a -----BEGIN RSA PRIVATE KEY-----\nx\n-----END RSA PRIVATE KEY----- b"),
            "a [REDACTED PRIVATE KEY] b"
        );
        assert_eq!(redact_text("düz metin"), "düz metin");
    }
}
