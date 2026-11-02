//! Büyük yapıştırma (Python `paste.py`).
//!
//! Kullanıcı yüzlerce satırlık derleme, test ya da tarama çıktısını görev metni
//! olarak yapıştırır; bu metin geçmişe girip her adımda yeniden gönderilir.
//! Sınırı aşan metin (`EVREN_PASTE_MAX_LINES` / `EVREN_PASTE_MAX_CHARS`)
//! `.evren/tmp/paste-N.txt` dosyasına yazılır, modele özeti gider: işaret
//! satırı, ilk ve son satırlar, ortadaki hata benzeri satırlar ve tamamının
//! `view_file` ile okunabileceği notu.
//!
//! Metin buraya gelmeden önce gizli kasadan geçmiş olmalı; diskteki dosyada
//! sırlar değil yer tutucular durur.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use evren_core::config::{ensure_evren_dirs, get_tmp_dir};

use crate::compaction::unreachable_regex;

pub const HEAD_LINES: usize = 15;
pub const TAIL_LINES: usize = 15;
pub const MAX_IMPORTANT_LINES: usize = 60;
pub const LINE_CAP: usize = 300;
/// Baş + orta + son gövdesi için yumuşak sınır (~3K token); işaret ve kapanış
/// notu bunun üstüne gelir.
pub const BODY_BUDGET: usize = 10_000;

fn important_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)error|fail|exception|traceback|warning CS|CVE-|critical|\bhigh\b|panic|fatal",
        )
        .unwrap_or_else(|_| unreachable_regex())
    })
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// Satır sonlarını \n yapar; (normalize metin, satırları) döner.
fn split_lines(text: &str) -> (String, Vec<String>) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_owned).collect();
    if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    (normalized, lines)
}

fn clip(line: &str) -> String {
    let line = line.trim_end();
    let n = chars(line);
    if n <= LINE_CAP {
        return line.to_string();
    }
    let head: String = line.chars().take(LINE_CAP).collect();
    format!("{head}... [+{} karakter]", n - LINE_CAP)
}

fn relative_path(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(".evren/tmp/{name}")
}

fn next_number(tmp_dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(tmp_dir) else {
        return 1;
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix("paste-")?
                .strip_suffix(".txt")?
                .parse::<u64>()
                .ok()
        })
        .max()
        .unwrap_or(0)
        + 1
}

/// Tam metni sıradaki boş `paste-N.txt` dosyasına yazar; olmazsa `None`.
fn save_paste(normalized: &str, workspace_root: &Path) -> Option<(u64, PathBuf)> {
    ensure_evren_dirs(workspace_root).ok()?;
    let tmp_dir = get_tmp_dir(workspace_root);
    std::fs::create_dir_all(&tmp_dir).ok()?;
    let mut number = next_number(&tmp_dir);
    loop {
        let path = tmp_dir.join(format!("paste-{number}.txt"));
        // create_new: eşzamanlı oturumun yazdığı dosyanın üstüne yazmaz.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(normalized.as_bytes()).ok()?;
                return Some((number, path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => number += 1,
            Err(_) => return None,
        }
    }
}

/// Ortadaki ayrık, hata benzeri satırlar (özgün sırayla).
fn important_lines(middle: &[String], seen: &mut std::collections::HashSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    for line in middle {
        let key = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if key.is_empty() || seen.contains(&key) || !important_re().is_match(line) {
            continue;
        }
        seen.insert(key);
        found.push(clip(line));
    }
    found
}

fn render(lines: &[String], total_chars: usize, number: u64, rel_path: &str) -> String {
    let total = lines.len();
    let marker = format!(
        "[Yapıştırılan metin #{number}: {total} satır, {total_chars} karakter → {rel_path}]"
    );
    let note = format!(
        "[EVREN] Metnin tamamı {rel_path} dosyasında. Ayrıntı gerekirse view_file ile bu \
         yoldan oku (satır aralığı vererek, bölüm bölüm); yapıştırılan metni yeniden yazma."
    );
    let (head, middle, tail): (&[String], &[String], &[String]) =
        if total <= HEAD_LINES + TAIL_LINES {
            (lines, &[], &[])
        } else {
            (
                &lines[..HEAD_LINES],
                &lines[HEAD_LINES..total - TAIL_LINES],
                &lines[total - TAIL_LINES..],
            )
        };

    let mut parts = vec![marker];
    parts.push(if tail.is_empty() {
        "Satırlar (uzun satırlar kısaltıldı):".to_string()
    } else {
        format!("İlk {} satır:", head.len())
    });
    parts.extend(head.iter().map(|l| clip(l)));
    let omitted_at = parts.len();
    if !tail.is_empty() {
        parts.push(format!("Son {} satır:", tail.len()));
        parts.extend(tail.iter().map(|l| clip(l)));
    }
    parts.push(note);

    if !middle.is_empty() {
        let mut seen = head
            .iter()
            .chain(tail)
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        let important = important_lines(middle, &mut seen);
        // Gövde = işaret ile kapanış notu arasındaki her şey.
        let mut used: usize = parts[1..parts.len() - 1].iter().map(|p| chars(p) + 1).sum();
        let mut chosen = Vec::new();
        for line in important.iter().take(MAX_IMPORTANT_LINES) {
            let n = chars(line) + 1;
            if used + n > BODY_BUDGET {
                break;
            }
            chosen.push(line.clone());
            used += n;
        }
        let mut header = format!("... {} satır atlandı.", middle.len());
        if !chosen.is_empty() {
            header.push_str(&format!(
                " Orta bölümdeki hata/uyarı satırları ({}/{}):",
                chosen.len(),
                important.len()
            ));
        }
        let mut insert = vec![header];
        insert.extend(chosen);
        parts.splice(omitted_at..omitted_at, insert);
    }
    parts.join("\n")
}

/// Sınırı aşan yapıştırmayı kısaltır: (modele gidecek metin, kaydedilen dosya).
///
/// Sınır içindeki metin olduğu gibi `None` ile döner. Dosya yazılamazsa ya da
/// kısa biçim daha kısa değilse özgün metin döner: bilgi sessizce kaybolmaz.
pub fn compact_paste(
    text: &str,
    workspace_root: &Path,
    max_lines: usize,
    max_chars: usize,
) -> (String, Option<PathBuf>) {
    if text.is_empty() {
        return (String::new(), None);
    }
    let (normalized, lines) = split_lines(text);
    let total_chars = chars(&normalized);
    if lines.len() <= max_lines && total_chars <= max_chars {
        return (text.to_string(), None);
    }
    let Some((number, path)) = save_paste(&normalized, workspace_root) else {
        return (text.to_string(), None);
    };
    let compact = render(&lines, total_chars, number, &relative_path(&path));
    if chars(&compact) >= chars(text) {
        let _ = std::fs::remove_file(&path);
        return (text.to_string(), None);
    }
    (compact, Some(path))
}

/// Yapıştırma kısaltıldıktan sonra kullanıcıya gösterilen tek satırlık not.
pub fn paste_info(text: &str, path: &Path) -> String {
    let (normalized, lines) = split_lines(text);
    format!(
        "Yapıştırılan metin ({} satır, {} karakter) {} dosyasına kaydedildi; modele özeti gidiyor \
         (tamamı view_file ile okunabilir).",
        lines.len(),
        chars(&normalized),
        relative_path(path)
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const LINES: usize = 80;
    const CHARS: usize = 6000;

    fn build_log(lines: usize, errors: usize) -> String {
        let step = (lines / errors).max(1);
        let mut out = vec![
            "Build started 12:00:01".to_string(),
            "Restore complete (0.4s)".to_string(),
        ];
        for i in 0..lines {
            if i % step == 0 && i / step < errors {
                out.push(format!(
                    "/src/Portal.Api/Module{i}/Handler{i}.cs({i},12): error CS0246: \
                     The type or namespace name 'Foo{i}' could not be found [/src/Portal.Api/Portal.Api.csproj]"
                ));
            } else {
                out.push(format!(
                    "  Compiling Portal.Api.Module{i} -> bin/Debug/net8.0/Module{i}.dll (ok)"
                ));
            }
        }
        out.extend([
            "Build FAILED.".to_string(),
            format!("    {errors} Error(s)"),
            "Time Elapsed 00:00:42".to_string(),
        ]);
        out.join("\n")
    }

    fn pastes(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root.join(".evren/tmp"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("paste-"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn text_within_the_limits_passes_through_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let eighty = (0..80)
            .map(|i| format!("satır {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        for text in ["kısa görev", "", eighty.as_str()] {
            assert_eq!(
                compact_paste(text, root, LINES, CHARS),
                (text.to_string(), None)
            );
        }
        assert!(!root.join(".evren").exists());
        // Kısalmayacak metin de bırakılır (dosya kalmaz).
        let text = (0..60)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(compact_paste(&text, root, 40, CHARS), (text.clone(), None));
        assert!(pastes(root).is_empty());
    }

    #[test]
    fn unwritable_target_returns_the_original_text() {
        let dir = tempfile::tempdir().unwrap();
        // `.evren` bir dosya: dizin açılamaz.
        std::fs::write(dir.path().join(".evren"), "x").unwrap();
        let text = build_log(300, 5);
        assert_eq!(compact_paste(&text, dir.path(), LINES, CHARS), (text, None));
    }

    #[test]
    fn big_paste_shrinks_keeps_errors_and_saves_the_full_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let text = build_log(1327, 40);
        assert!(text.len() > 44_000);
        let (compact, path) = compact_paste(&text, root, LINES, CHARS);
        let path = path.unwrap();
        assert!(chars(&compact) <= 10_500, "{}", chars(&compact));
        assert!(compact.len() < text.len() / 4);
        for line in text.lines().filter(|l| l.contains("error CS0246")) {
            assert!(compact.contains(line), "{line}");
        }
        assert!(compact.contains("Build FAILED."));
        assert!(compact.contains("Build started 12:00:01"));
        assert!(!compact.contains("Compiling Portal.Api.Module700 "));
        let count = text.lines().count();
        assert!(compact.starts_with(&format!(
            "[Yapıştırılan metin #1: {count} satır, {} karakter → .evren/tmp/paste-1.txt]",
            chars(&text)
        )));
        assert!(compact.contains("view_file"));
        assert_eq!(path, root.join(".evren").join("tmp").join("paste-1.txt"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let second = build_log(900, 10);
        let (compact2, path2) = compact_paste(&second, root, LINES, CHARS);
        let path2 = path2.unwrap();
        assert_eq!(path2.file_name().unwrap(), "paste-2.txt");
        assert!(compact2.contains("#2"));
        assert_eq!(std::fs::read_to_string(&path2).unwrap(), second);
        let gi = std::fs::read_to_string(root.join(".evren/.gitignore")).unwrap();
        assert!(gi.lines().any(|l| l == "tmp/"));
    }

    #[test]
    fn error_like_lines_are_kept_deduplicated_and_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let long = format!("E{} error {}", "x".repeat(500), "y".repeat(500));
        let mut lines: Vec<String> = vec!["başlık".into(); 15];
        lines.extend(
            [
                "Traceback (most recent call last):",
                "Traceback (most recent call last):",
                "CVE-2024-1234 high severity in lodash",
                "CVE-2024-1234 high severity in lodash",
                "CVE-2024-1234 high severity in lodash",
                "Program.cs(12,5): warning CS8618: Non-nullable property",
                "PANIC: out of memory",
                "FATAL: disk full",
                "Test FAILED: test_login",
            ]
            .map(String::from),
        );
        lines.push(long);
        lines.extend((0..300).map(|i| format!("  ok step {i} passed quietly")));
        lines.extend(vec!["son".to_string(); 15]);
        let (compact, _) = compact_paste(&lines.join("\n"), root, LINES, CHARS);
        for needle in [
            "CVE-2024-1234 high severity in lodash",
            "warning CS8618",
            "PANIC: out of memory",
            "FATAL: disk full",
            "Test FAILED: test_login",
            "Traceback (most recent call last):",
        ] {
            assert_eq!(compact.matches(needle).count(), 1, "{needle}");
        }
        let long_line = compact.lines().find(|l| l.starts_with("Ex")).unwrap();
        assert!(chars(long_line) <= 300 + "... [+1007 karakter]".len());
        assert!(long_line.contains("karakter]"));
        assert!(!compact.contains("ok step 120 passed"));
        // Ortadan en çok altmış hata satırı, sayaçla.
        let mut lines: Vec<String> = vec!["üst".into(); 15];
        lines.extend((0..200).map(|i| format!("item {i}: error E{i:04} unique failure")));
        lines.extend(vec!["alt".to_string(); 15]);
        let (compact, _) = compact_paste(&lines.join("\n"), root, 50, CHARS);
        let shown: Vec<&str> = compact.lines().filter(|l| l.starts_with("item ")).collect();
        assert_eq!(shown.len(), 60);
        assert!(compact.contains("(60/200)"));
        assert_eq!(shown[0], "item 0: error E0000 unique failure");
    }

    #[test]
    fn char_budgets_clipping_and_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut lines: Vec<String> = vec!["üst".into(); 15];
        lines.extend((0..300).map(|i| format!("{i} error {}", "z".repeat(290))));
        lines.extend(vec!["alt".to_string(); 15]);
        let (compact, _) = compact_paste(&lines.join("\n"), root, 50, CHARS);
        assert!(chars(&compact) <= 11_000);
        // Uzun tek satır kırpılır ama tamamı kaydedilir.
        let text = "x".repeat(20_000);
        let (compact, path) = compact_paste(&text, root, LINES, CHARS);
        assert!(chars(&compact) < 1500);
        assert_eq!(std::fs::read_to_string(path.unwrap()).unwrap(), text);
        // Az ama çok uzun satır karakter sınırından kısaltılır.
        let text = (0..40)
            .map(|i| format!("{i} {}", "q".repeat(400)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.len() > 6000);
        let (compact, path) = compact_paste(&text, root, LINES, CHARS);
        assert!(path.is_some());
        assert!(compact.len() < text.len());
        // CRLF kayıtta normalize edilir.
        let text = (0..120)
            .map(|i| format!("satır {i} {}", "w".repeat(60)))
            .collect::<Vec<_>>()
            .join("\r\n")
            + "\r\n";
        let (compact, path) = compact_paste(&text, root, LINES, CHARS);
        assert!(!std::fs::read(path.unwrap()).unwrap().contains(&b'\r'));
        assert!(compact.contains("120 satır"));
        // Sınırlar parametreden gelir.
        let text = (0..120)
            .map(|i| format!("satır {i} {}", "v".repeat(60)))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            compact_paste(&text, root, 500, 50_000),
            (text.clone(), None)
        );
        assert!(compact_paste(&text, root, 100, 50_000).1.is_some());
    }

    fn session(ws: &Path) -> crate::AgentSession {
        let mut env = std::collections::BTreeMap::new();
        env.insert("EVREN_API_KEY".to_string(), "sk-test-dummy".to_string());
        env.insert("EVREN_DEFAULT_MODEL".to_string(), "fake-model".to_string());
        let config = evren_core::config::Config::from_env(env);
        let client = std::sync::Arc::new(evren_api::ApiClient::new(&config).unwrap());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        crate::AgentSession::new(
            client,
            config,
            ws,
            tx,
            tokio_util::sync::CancellationToken::new(),
        )
    }

    #[test]
    fn session_masks_secrets_before_the_paste_is_written_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = session(dir.path());
        let token = format!("fake{}", "TokenForTests0123456789abcd");
        let mut lines: Vec<String> = (0..400)
            .map(|i| format!("  Compiling Module{i} -> ok"))
            .collect();
        lines[200] = format!("curl failed: error 401 with -H \"Authorization: Bearer {token}\"");
        let sent = s.compact_user_text(&lines.join(
            "
",
        ));
        assert!(sent.starts_with("[Yapıştırılan metin #1:"));
        assert!(
            !sent.contains(&token) && sent.contains("{{EVREN_SECRET_1}}"),
            "{sent}"
        );
        let saved = std::fs::read_to_string(dir.path().join(".evren/tmp/paste-1.txt")).unwrap();
        assert!(!saved.contains(&token));
        assert!(saved.contains("Bearer {{EVREN_SECRET_1}}"));
        // Küçük metin olduğu gibi kalır, dosya yazılmaz.
        assert_eq!(
            s.compact_user_text("testleri çalıştır"),
            "testleri çalıştır"
        );
        assert_eq!(pastes(dir.path()).len(), 1);
    }

    #[test]
    fn paste_info_is_a_one_line_turkish_notice() {
        let dir = tempfile::tempdir().unwrap();
        let text = build_log(300, 5);
        let (_, path) = compact_paste(&text, dir.path(), LINES, CHARS);
        let info = paste_info(&text, &path.unwrap());
        assert!(!info.contains('\n'));
        assert!(info.contains(".evren/tmp/paste-1.txt"));
        assert!(info.contains("satır"));
    }
}
