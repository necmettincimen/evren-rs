//! `@dosya` / `@dosya:10-40` / `@dosya#L10-L40` bahislerini metne ekler
//! (Python `extras.expand_mentions` paritesi).

use std::path::{Component, Path, PathBuf};

use evren_core::texts;
use fancy_regex::Regex;

/// Bir bahiste eklenen en çok satır (`MENTION_MAX_LINES`).
pub const MENTION_MAX_LINES: usize = 400;

const IMAGE_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

#[allow(clippy::expect_used)]
fn mention_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?<![\w@])@([\w./\-]+)(?:(?::|#L)(\d+)-L?(\d+))?").expect("sabit regex")
    })
}

/// `..` ve `.` bileşenlerini sözcüksel olarak sadeleştirir (var olmayan yol için).
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `Path.resolve()` benzeri: var olan yol için `canonicalize`, yoksa sözcüksel.
fn resolve(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| lexical_normalize(p))
}

/// Metindeki bahisleri genişletir; `(yeni metin, uyarılar)` döner.
pub fn expand_mentions(text: &str, workspace_root: &Path) -> (String, Vec<String>) {
    let root = resolve(workspace_root);
    let mut blocks: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut seen: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for caps in mention_re().captures_iter(text).flatten() {
        let rel = caps
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or("")
            .trim_end_matches(['.', ',', ';'])
            .to_string();
        let start = caps.get(2).map(|m| m.as_str().to_string());
        let end = caps.get(3).map(|m| m.as_str().to_string());
        let key = (rel.clone(), start.clone(), end.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let path = resolve(&root.join(&rel));
        if !path.starts_with(&root) {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .unwrap_or_default();
        if IMAGE_EXTS.contains(&ext.as_str()) && path.is_file() {
            blocks.push(texts::mention_image(&rel));
            continue;
        }
        if !path.is_file() {
            // "@kullanici" gibi düz sözcükler bahis değildir; yalnız yol benzerleri uyarır.
            if rel.contains('.') || rel.contains('/') || rel.contains('\\') {
                warnings.push(texts::mention_not_found(&rel));
            }
            continue;
        }
        let lines: Vec<String> = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::to_string)
                .collect(),
            Err(e) => {
                warnings.push(texts::mention_unreadable(&rel, &e));
                continue;
            }
        };
        let first: usize = match start.as_deref().and_then(|s| s.parse::<usize>().ok()) {
            Some(s) => s.max(1),
            None => 1,
        };
        let last: usize = match end.as_deref().and_then(|s| s.parse::<usize>().ok()) {
            Some(e) => e.min(lines.len()),
            None => lines.len(),
        };
        let mut chosen: Vec<&String> = if first - 1 < last {
            lines[first - 1..last].iter().collect()
        } else {
            Vec::new()
        };
        let mut note = String::new();
        if chosen.len() > MENTION_MAX_LINES {
            chosen.truncate(MENTION_MAX_LINES);
            note = texts::mention_line_limit_note(MENTION_MAX_LINES);
        }
        let body = chosen
            .iter()
            .enumerate()
            .map(|(i, ln)| format!("{}: {ln}", first + i))
            .collect::<Vec<_>>()
            .join("\n");
        let last_shown = (first + chosen.len()).saturating_sub(1);
        blocks.push(format!(
            "{}\n{body}{note}",
            texts::mention_block_header(&rel, first, last_shown)
        ));
    }
    if blocks.is_empty() {
        return (text.to_string(), warnings);
    }
    (
        format!(
            "{text}\n\n{}\n{}",
            texts::MENTION_ATTACHED_HEADER,
            blocks.join("\n\n")
        ),
        warnings,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn expands_ranges_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "bir\niki\nüç\n").unwrap();
        let (out, warns) = expand_mentions("bak @a.txt:2-3 ve @yok.txt, @kullanici", dir.path());
        assert_eq!(
            out,
            "bak @a.txt:2-3 ve @yok.txt, @kullanici\n\n[Eklenen dosyalar]\n[@a.txt satır 2-3]\n2: iki\n3: üç"
        );
        assert_eq!(warns, vec!["@yok.txt bulunamadı."]);

        let (out, warns) = expand_mentions("@a.txt#L1-L1 @a.txt#L1-L1", dir.path());
        assert!(out.ends_with("[@a.txt satır 1-1]\n1: bir"));
        assert!(warns.is_empty());

        // e-posta adresleri ve @@ bahis değildir; kök dışı yollar atlanır.
        let (out, warns) = expand_mentions("x@a.txt @@a.txt @../a.txt", dir.path());
        assert_eq!(out, "x@a.txt @@a.txt @../a.txt");
        assert!(warns.is_empty());
    }

    #[test]
    fn line_limit_and_images() {
        let dir = tempfile::tempdir().unwrap();
        let big: String = (0..450).map(|i| format!("l{i}\n")).collect();
        std::fs::write(dir.path().join("b.txt"), big).unwrap();
        std::fs::write(dir.path().join("p.png"), b"x").unwrap();
        let (out, _) = expand_mentions("@b.txt @p.png", dir.path());
        assert!(out.contains("[@b.txt satır 1-400]\n1: l0\n"));
        assert!(out.ends_with(
            "400: l399\n... [400 satır sınırı]\n\n[@p.png] görsel dosya: içeriği için view_file aracını kullan."
        ));
    }
}
