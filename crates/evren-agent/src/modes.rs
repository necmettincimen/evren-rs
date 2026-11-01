//! Çalışma kipi yardımcıları (Python `modes.py`).
//!
//! Kip türü `evren-core`'daki [`Mode`] ile paylaşılır; burada yalnızca
//! metin öneki ayrıştırma ve etiketleme vardır.

pub use evren_core::tools::Mode;

/// `"ask: ..."`, `"plan: ..."`, `"/ask ..."`, `"/plan ..."` önekleri.
const MODE_PREFIXES: &[(&str, Mode)] = &[
    ("ask:", Mode::Ask),
    ("plan:", Mode::Plan),
    ("/ask ", Mode::Ask),
    ("/plan ", Mode::Plan),
];

/// Metnin başındaki kip önekini ayıklar: `(kip, kalan metin)`.
/// Önek yoksa `(None, metin)` döner (metin olduğu gibi).
pub fn parse_mode_prefix(text: &str) -> (Option<Mode>, String) {
    let stripped = text.trim_start();
    let lowered = stripped.to_lowercase();
    for (prefix, mode) in MODE_PREFIXES {
        if lowered.starts_with(prefix) {
            // Önekler ASCII olduğundan `to_lowercase` uzunluğu korur.
            let rest = stripped.get(prefix.len()..).unwrap_or("").trim();
            return (Some(*mode), rest.to_string());
        }
    }
    (None, text.to_string())
}

/// Kullanıcıya gösterilen kip etiketi.
pub fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "normal",
        Mode::Ask => "ask (salt-okuma)",
        Mode::Plan => "plan (yalnızca plan yazımı)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_parsing() {
        assert_eq!(
            parse_mode_prefix("  ask: dosyaları say"),
            (Some(Mode::Ask), "dosyaları say".into())
        );
        assert_eq!(
            parse_mode_prefix("/PLAN  x"),
            (Some(Mode::Plan), "x".into())
        );
        assert_eq!(parse_mode_prefix("plan yap"), (None, "plan yap".into()));
        assert_eq!(parse_mode_prefix("ask:"), (Some(Mode::Ask), String::new()));
    }

    #[test]
    fn labels() {
        assert_eq!(mode_label(Mode::Plan), "plan (yalnızca plan yazımı)");
    }
}
