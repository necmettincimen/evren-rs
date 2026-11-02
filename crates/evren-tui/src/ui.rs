//! Düz metin çıktı yardımcıları (Python `ui.py`, rich olmayan dal).

use std::io::{IsTerminal, Write};
use std::path::Path;

use serde_json::{Map, Value};

/// Uçbirime renk basılabilir mi (`NO_COLOR` kapatır).
pub fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn paint(code: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn print_info(msg: &str) {
    println!("{} {msg}", paint("36", "[*]"));
}

pub fn print_success(msg: &str) {
    println!("{} {msg}", paint("1;32", "[+]"));
}

pub fn print_warning(msg: &str) {
    println!("{} {msg}", paint("33", "[!]"));
}

pub fn print_error(msg: &str) {
    println!("{} {msg}", paint("1;31", "[X]"));
}

pub fn flush() {
    let _ = std::io::stdout().flush();
}

/// Binlik ayraçlı sayı (`f"{n:,}"`).
pub fn thousands(n: i64) -> String {
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

/// `_shorten_path`: ev dizini `~`, uzun yollar `…` ile kırpılır.
pub fn shorten_path(path: &str, limit: usize) -> String {
    let mut text = path.trim().to_string();
    if text.is_empty() {
        return text;
    }
    if let Some(home) = dirs::home_dir() {
        let home = home.display().to_string();
        let sep = if text.contains('\\') { "\\" } else { "/" };
        if text == home {
            text = "~".to_string();
        } else if text
            .to_lowercase()
            .starts_with(&format!("{}{sep}", home.to_lowercase()))
        {
            text = format!("~{}", text.get(home.len()..).unwrap_or(""));
        }
    }
    let count = text.chars().count();
    if count > limit {
        let tail: String = text.chars().skip(count - (limit - 1)).collect();
        text = format!("…{tail}");
    }
    text
}

fn short_name(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// `_config_line`: tek satır yapılandırma kaynağı özeti (anahtar değeri yok).
pub fn config_line(info: &Map<String, Value>) -> String {
    let get = |k: &str| info.get(k).and_then(Value::as_str).unwrap_or("");
    let env_file = get("env_file");
    let mut key_source = get("key_source").to_string();
    if let Some((head, _)) = key_source.split_once('(') {
        key_source = head.trim().to_string();
    }
    let key_part = if !env_file.is_empty() && key_source.to_lowercase() == env_file.to_lowercase()
    {
        ".env".to_string()
    } else {
        let n = short_name(&key_source);
        if n.is_empty() {
            "bulunamadı".to_string()
        } else {
            n
        }
    };
    let or_yok = |s: String| if s.is_empty() { "yok".to_string() } else { s };
    let missing = if info
        .get("config_json_exists")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        ""
    } else {
        " (yok)"
    };
    let count = info.get("key_count").and_then(Value::as_i64).unwrap_or(0);
    format!(
        "Yapılandırma: .env={} | config={}{missing} | anahtar={key_part} ({count} anahtar)",
        or_yok(short_name(env_file)),
        or_yok(short_name(get("config_json"))),
    )
}

/// Depo kökündeki `CHANGELOG.md` (ikiliye gömülü).
pub const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// `get_changelog_notes`: `## [sürüm]` başlığı altındaki `- ` / `* ` maddeleri.
pub fn changelog_notes(text: &str, version: &str, max_items: usize) -> Vec<String> {
    let heading = format!(r"(?m)^##\s+\[{}\].*$", regex::escape(version));
    let Ok(re) = regex::Regex::new(&heading) else {
        return Vec::new();
    };
    let Some(m) = re.find(text) else {
        return Vec::new();
    };
    let mut section = &text[m.end()..];
    if let Some(next) = regex::Regex::new(r"(?m)^##\s+")
        .ok()
        .and_then(|r| r.find(section))
    {
        section = &section[..next.start()];
    }
    let mut notes = Vec::new();
    for line in section.lines() {
        let stripped = line.trim();
        if let Some(rest) = stripped
            .strip_prefix("- ")
            .or_else(|| stripped.strip_prefix("* "))
        {
            notes.push(rest.trim().to_string());
            if notes.len() >= max_items {
                break;
            }
        }
    }
    notes
}

/// Bu sürümün notları (gömülü CHANGELOG'dan).
pub fn release_notes(max_items: usize) -> Vec<String> {
    changelog_notes(CHANGELOG, evren_core::VERSION, max_items)
}

/// `strip_markdown`: **kalın**, `kod`, # başlık işaretlerini kaldırır, madde → `• `.
pub fn strip_markdown(text: &str) -> String {
    static RES: std::sync::OnceLock<Option<[regex::Regex; 4]>> = std::sync::OnceLock::new();
    let Some([code, bold, heading, bullet]) = RES.get_or_init(|| {
        Some([
            regex::Regex::new(r"`{1,3}[^`]*`{1,3}").ok()?,
            regex::Regex::new(r"(\*\*|__)").ok()?,
            regex::Regex::new(r"(?m)^\s{0,3}#{1,6}\s+").ok()?,
            regex::Regex::new(r"(?m)^(\s*)[-*]\s+").ok()?,
        ])
    }) else {
        return text.to_string();
    };
    let text = code.replace_all(text, |c: &regex::Captures| {
        c[0].trim_matches('`').to_string()
    });
    let text = bold.replace_all(&text, "");
    let text = heading.replace_all(&text, "");
    bullet.replace_all(&text, "$1• ").into_owned()
}

/// `_clean_release_note`: markdown'sız, tek satır, `limit` karakterde kelime sınırında `…`.
pub fn clean_release_note(note: &str, limit: usize) -> String {
    let text = strip_markdown(note);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut text = text
        .trim_start_matches(['#', '-', '•', ' '])
        .trim()
        .to_string();
    if text.chars().count() > limit {
        let mut cut: String = text.chars().take(limit).collect();
        if let Some(pos) = cut.rfind(' ') {
            cut.truncate(pos);
        }
        text = format!("{}…", cut.trim_end_matches([' ', ',', ';', ':', '.', '·']));
    }
    text
}

/// `print_banner` (düz metin dalı).
pub fn print_banner(
    version: &str,
    model: &str,
    workspace: &Path,
    quota: &str,
    config: &Map<String, Value>,
    notes: &[String],
) {
    let rule = "=".repeat(70);
    println!("{rule}");
    println!("✦ EVREN CLI v{version} - AI Development & Project Assistant");
    println!(
        "Model: {model} | Çalışma Alanı: {}",
        shorten_path(&workspace.display().to_string(), 48)
    );
    if !quota.is_empty() {
        println!("Kalan Kota: {quota}");
    }
    if !config.is_empty() {
        println!("{}", config_line(config));
    }
    if !notes.is_empty() {
        println!("\nv{version} Sürüm Notları:");
        for note in notes {
            println!("  • {note}");
        }
    }
    println!("{rule}");
}

/// Python `repr` benzeri kısa değer gösterimi.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => {
            if s.contains('\'') && !s.contains('"') {
                format!("\"{s}\"")
            } else {
                format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
            }
            .replace('\n', "\\n")
        }
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("'{k}': {}", py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(n) => n.to_string(),
    }
}

/// `format_tool_args`: boş/yanlış değerler atlanır, kısa olan önce, 100 karakter sınırı.
pub fn format_tool_args(args: &Value) -> String {
    let Some(map) = args.as_object() else {
        return String::new();
    };
    let mut parts: Vec<(String, String)> = map
        .iter()
        .filter(|(_, v)| match v {
            Value::Null | Value::Bool(false) => false,
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        })
        .map(|(k, v)| (k.clone(), py_repr(v)))
        .collect();
    parts.sort_by_key(|(_, r)| r.chars().count());
    let joined = parts
        .iter()
        .map(|(k, r)| format!("{k}={r}"))
        .collect::<Vec<_>>()
        .join(", ");
    if joined.chars().count() > 100 {
        format!("{}...", joined.chars().take(97).collect::<String>())
    } else {
        joined
    }
}

pub fn print_tool_call(name: &str, args: &Value) {
    println!(
        "{}",
        paint("1;34", &format!(">> [Araç] {name}({})", format_tool_args(args)))
    );
}

pub fn print_tool_result(name: &str, ok: bool, summary: &str) {
    if ok {
        println!("  {} {name} completed: {summary}", paint("32", "[+]"));
    } else {
        println!("  {} {name} error: {summary}", paint("31", "[X]"));
    }
}

pub fn print_run_summary(steps: u32, seconds: f64, tokens: i64) {
    println!(
        "{}",
        paint(
            "2",
            &format!("Özet: {steps} adım • {seconds:.1}s • {tokens} token")
        )
    );
}

/// Birleşik diff metni mi (`--- a/` + `+++ b/` başlıkları).
pub fn looks_like_diff(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("--- ")) && text.lines().any(|l| l.starts_with("+++ "))
}

/// `print_diff` (düz dal): `+` yeşil, `-` kırmızı.
pub fn print_diff(text: &str) {
    if text.trim().is_empty() {
        print_info("Değişiklik yok.");
        return;
    }
    for line in text.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            println!("{}", paint("1", line));
        } else if line.starts_with('+') {
            println!("{}", paint("32", line));
        } else if line.starts_with('-') {
            println!("{}", paint("31", line));
        } else if line.starts_with("@@") {
            println!("{}", paint("36", line));
        } else {
            println!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn thousands_groups() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(-1000), "-1,000");
    }

    #[test]
    fn tool_args_skip_empty_and_sort() {
        let args = json!({"path": "a.txt", "content": "uzun içerik", "flag": false, "x": null});
        assert_eq!(format_tool_args(&args), "path='a.txt', content='uzun içerik'");
    }

    #[test]
    fn tool_args_clip_long() {
        let args = json!({"content": "x".repeat(200)});
        let s = format_tool_args(&args);
        assert_eq!(s.chars().count(), 100);
        assert!(s.ends_with("..."));
    }

    #[test]
    fn shorten_long_path() {
        let p = format!("D:/{}", "a".repeat(80));
        let s = shorten_path(&p, 48);
        assert_eq!(s.chars().count(), 48);
        assert!(s.starts_with('…'));
    }

    #[test]
    fn config_line_without_keys() {
        let mut m = Map::new();
        m.insert("env_file".into(), json!(""));
        m.insert("config_json".into(), json!("C:/x/config.json"));
        m.insert("config_json_exists".into(), json!(false));
        m.insert("key_count".into(), json!(0));
        assert_eq!(
            config_line(&m),
            "Yapılandırma: .env=yok | config=config.json (yok) | anahtar=bulunamadı (0 anahtar)"
        );
    }

    #[test]
    fn changelog_section_notes() {
        let text = "# Log\n\n## [1.2.0] - x\n\n### Eklendi\n- bir\n* iki\n\n## [1.1.0]\n- eski\n";
        assert_eq!(changelog_notes(text, "1.2.0", 6), vec!["bir", "iki"]);
        assert_eq!(changelog_notes(text, "1.2.0", 1), vec!["bir"]);
        assert!(changelog_notes(text, "9.9.9", 6).is_empty());
        assert!(!release_notes(4).is_empty());
    }

    #[test]
    fn release_note_cleaned() {
        assert_eq!(clean_release_note("**Kalın** `kod` not", 56), "Kalın kod not");
        let long = "kelime ".repeat(20);
        let out = clean_release_note(&long, 56);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 57);
        assert_eq!(strip_markdown("# Başlık\n- madde"), "Başlık\n• madde");
    }

    #[test]
    fn diff_detection() {
        assert!(looks_like_diff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n"));
        assert!(!looks_like_diff("dosya yazıldı"));
    }
}
