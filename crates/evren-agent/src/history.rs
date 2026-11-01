//! Komut geçmişi dosyası (`~/.evren/history.txt`), prompt_toolkit `FileHistory`
//! biçimi: her girdi `\n# <zaman>\n` başlığı ve `+` ön ekli satırlarla eklenir.
//! Okumada `+` satırları birleştirilir, en yeni girdi önce döner.

use std::io::Write;
use std::path::Path;

use evren_core::error::EvrenError;

/// Dosyadaki girdileri en yeni önce döndürür (dosya yoksa boş).
pub fn load_history_strings(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let mut strings: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let flush = |lines: &mut Vec<String>, strings: &mut Vec<String>| {
        if !lines.is_empty() {
            let mut joined: String = lines.concat();
            // Son satır sonunu düşür (prompt_toolkit `[:-1]`).
            joined.pop();
            strings.push(joined);
            lines.clear();
        }
    };
    for raw in bytes.split_inclusive(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        if let Some(rest) = line.strip_prefix('+') {
            lines.push(rest.to_string());
        } else {
            flush(&mut lines, &mut strings);
        }
    }
    flush(&mut lines, &mut strings);
    strings.reverse();
    strings
}

/// `str(datetime.datetime.now())`: mikrosaniye sıfırsa kesir yazılmaz.
fn now_text() -> String {
    let now = chrono::Local::now();
    if now.format("%f").to_string() == "000000000" {
        now.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        now.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
    }
}

/// Girdiyi dosyanın sonuna ekler (`\n# <zaman>\n` + `+satır\n`...).
pub fn store_string(path: &Path, text: &str) -> Result<(), EvrenError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EvrenError::io(parent, e))?;
    }
    let mut out = format!("\n# {}\n", now_text());
    for line in text.split('\n') {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| EvrenError::io(path, e))?;
    f.write_all(out.as_bytes())
        .map_err(|e| EvrenError::io(path, e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h/history.txt");
        assert!(load_history_strings(&p).is_empty());
        store_string(&p, "ilk komut").unwrap();
        store_string(&p, "çok\nsatırlı ğ").unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.starts_with("\n# 20"));
        assert!(raw.ends_with("+çok\n+satırlı ğ\n"));
        assert_eq!(
            load_history_strings(&p),
            vec!["çok\nsatırlı ğ", "ilk komut"]
        );
    }
}
