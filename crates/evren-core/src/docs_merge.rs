//! İşaretli yönetilen bölüm birleştirme (Python `docs_merge.py`). Belge sıradan
//! metin ile HTML yorum işaretleri arasındaki *yönetilen bölümlerden* oluşur:
//!
//! ```text
//! <!-- evren:managed:start section=runtime_env -->
//! ...CLI'nin yeniden yazabileceği içerik...
//! <!-- evren:managed:end section=runtime_env -->
//! ```
//!
//! Kurallar (yalnız metin işleme, dosya G/Ç yok): yalnız yönetilen bölümün içi
//! yeniden yazılır; işaretsiz belgeye dokunulmaz; bozuk işaretler (iç içe,
//! bilinmeyen kapanış, kapanmamış) belgeyi bu çalıştırma için salt-okunur yapar;
//! CRLF/LF ve baştaki BOM korunur.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

pub const MARKER_PREFIX: &str = "evren:managed";

/// Belgeye dokunulmaması gerektiğinde dönen hata metinleri.
pub const UNTOUCHABLE: &str = "dokümanda yönetilen blok yok";
pub const MALFORMED: &str = "bozuk iç içe yönetilen blok işareti";

/// Bir yönetilen bölümün satır aralığı (işaret satırları dahil).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionSpan {
    pub name: String,
    /// Başlangıç işaret satırının indeksi.
    pub start_line: usize,
    /// Bitiş işaret satırının indeksi.
    pub end_line: usize,
    pub body: Vec<String>,
}

impl SectionSpan {
    pub fn body_text(&self) -> String {
        self.body.join("\n")
    }
}

fn marker_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^[ \t]*<!--[ \t]*evren:managed:(start|end)[ \t]+section=([A-Za-z0-9_-]+)[ \t]*-->[ \t]*$",
        )
        .ok()
    })
    .as_ref()
}

/// Bölüm adları küçük harf, kısa ve işaret için güvenlidir.
pub fn is_valid_section_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let low = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    low(first) && name.len() <= 40 && chars.all(|c| low(c) || c == '_' || c == '-')
}

/// Başlangıç işareti; geçersiz adda `None` (Python `MergeError`).
pub fn start_marker(name: &str) -> Option<String> {
    is_valid_section_name(name).then(|| format!("<!-- {MARKER_PREFIX}:start section={name} -->"))
}

pub fn end_marker(name: &str) -> Option<String> {
    is_valid_section_name(name).then(|| format!("<!-- {MARKER_PREFIX}:end section={name} -->"))
}

/// Tam bir yönetilen bölüm bloğu (sonda satır sonu yok).
pub fn wrap_section(name: &str, body: &str) -> Option<String> {
    let mut lines = vec![start_marker(name)?];
    lines.extend(body_lines(body));
    lines.push(end_marker(name)?);
    Some(lines.join("\n"))
}

/// Boş yazımları atlamak için kararlı içerik parmak izi (gövde gibi normalize edilir).
pub fn section_hash(body: &str) -> String {
    let normalized = body_lines(body).join("\n");
    let digest = Sha256::digest(normalized.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Gövdeyi baş/son boş satırları atılmış satırlara çevirir.
fn body_lines(body: &str) -> Vec<String> {
    let mut lines: Vec<String> = normalize_newlines(body)
        .split('\n')
        .map(str::to_string)
        .collect();
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
}

struct Split {
    lines: Vec<String>,
    newline: &'static str,
    bom: bool,
}

fn split(text: &str) -> Split {
    let bom = text.starts_with('\u{feff}');
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let newline = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let lines = normalize_newlines(body)
        .split('\n')
        .map(str::to_string)
        .collect();
    Split {
        lines,
        newline,
        bom,
    }
}

fn join(lines: &[String], newline: &str, bom: bool) -> String {
    let text = lines.join(newline);
    if bom {
        format!("\u{feff}{text}")
    } else {
        text
    }
}

/// (bölümler, hatalar). Hata varsa belge salt-okunurdur.
pub fn parse_sections(text: &str) -> (Vec<SectionSpan>, Vec<String>) {
    let Split { lines, .. } = split(text);
    let mut sections: Vec<SectionSpan> = Vec::new();
    let mut open: Option<(String, usize)> = None;
    let Some(re) = marker_re() else {
        return (sections, Vec::new());
    };
    for (index, line) in lines.iter().enumerate() {
        let Some(caps) = re.captures(line) else {
            continue;
        };
        let kind = &caps[1];
        let name = caps[2].to_string();
        let n = index + 1;
        if kind == "start" {
            if let Some((open_name, _)) = &open {
                return (
                    vec![],
                    vec![format!(
                        "satır {n}: '{name}' bloğu '{open_name}' bloğu içinde açıldı"
                    )],
                );
            }
            if !is_valid_section_name(&name) {
                return (
                    vec![],
                    vec![format!("satır {n}: geçersiz bölüm adı '{name}'")],
                );
            }
            if sections.iter().any(|s| s.name == name) {
                return (
                    vec![],
                    vec![format!("satır {n}: '{name}' bölümü tekrar ediyor")],
                );
            }
            open = Some((name, index));
            continue;
        }
        let Some((open_name, open_index)) = open.take() else {
            return (
                vec![],
                vec![format!("satır {n}: açık blok olmadan '{name}' kapatıldı")],
            );
        };
        if name != open_name {
            return (
                vec![],
                vec![format!(
                    "satır {n}: '{name}' kapatıldı ama açık blok '{open_name}'"
                )],
            );
        }
        sections.push(SectionSpan {
            name: open_name,
            start_line: open_index,
            end_line: index,
            body: lines[open_index + 1..index].to_vec(),
        });
    }
    if let Some((open_name, open_index)) = open {
        return (
            vec![],
            vec![format!(
                "satır {}: '{open_name}' bloğu kapatılmadı",
                open_index + 1
            )],
        );
    }
    (sections, Vec::new())
}

/// Belge CLI işaretleri taşıyorsa `true`.
pub fn has_managed_sections(text: &str) -> bool {
    let (sections, errors) = parse_sections(text);
    !sections.is_empty() && errors.is_empty()
}

/// Bölüm adı → gövde. İşaretler bozuksa boş.
pub fn extract_sections(text: &str) -> BTreeMap<String, String> {
    let (sections, errors) = parse_sections(text);
    if !errors.is_empty() {
        return BTreeMap::new();
    }
    sections
        .into_iter()
        .map(|s| (s.name.clone(), s.body_text()))
        .collect()
}

/// Sonuç: (yeni metin, değişen adlar, hatalar). Hata varsa metin girdiyle aynıdır.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub changed: Vec<String>,
    pub errors: Vec<String>,
}

/// Yönetilen gövdeleri değiştirir; `adopt` ile eksik olanları sona ekler.
/// Geçersiz bölüm adları sessizce atlanır (Python `MergeError` programcı hatasıdır).
pub fn render_sections(
    text: &str,
    sections: &BTreeMap<String, String>,
    adopt: bool,
    adopt_order: &[&str],
) -> Rendered {
    let unchanged = |errors: Vec<String>| Rendered {
        text: text.to_string(),
        changed: Vec::new(),
        errors,
    };
    let requested: BTreeMap<&str, &str> = sections
        .iter()
        .filter(|(k, _)| is_valid_section_name(k))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let (existing, errors) = parse_sections(text);
    if !errors.is_empty() {
        return unchanged(vec![MALFORMED.to_string()]);
    }
    if existing.is_empty() && !adopt {
        return unchanged(vec![UNTOUCHABLE.to_string()]);
    }
    if requested.is_empty() && !adopt {
        return unchanged(Vec::new());
    }

    let Split {
        mut lines,
        newline,
        bom,
    } = split(text);
    let mut changed: Vec<String> = Vec::new();

    // Önceki indeksler geçerli kalsın diye sondan başa değiştirilir.
    let mut spans: Vec<&SectionSpan> = existing.iter().collect();
    spans.sort_by_key(|s| std::cmp::Reverse(s.start_line));
    for span in spans {
        let Some(body) = requested.get(span.name.as_str()) else {
            continue;
        };
        let new_body = body_lines(body);
        if span.body == new_body {
            continue;
        }
        lines.splice(span.start_line + 1..span.end_line, new_body);
        changed.push(span.name.clone());
    }

    // Yönetilen belgeler yeni bölüm kabul eder (yeni CLI sürümü ekleyebilir).
    let missing: Vec<&str> = requested
        .keys()
        .copied()
        .filter(|name| !existing.iter().any(|s| s.name == *name))
        .collect();
    if !missing.is_empty() {
        let mut order: Vec<&str> = adopt_order.to_vec();
        order.extend(missing.iter().copied().filter(|n| !adopt_order.contains(n)));
        for name in order {
            if !missing.contains(&name) {
                continue;
            }
            let (Some(start), Some(end)) = (start_marker(name), end_marker(name)) else {
                continue;
            };
            let mut block = vec![start];
            block.extend(body_lines(requested.get(name).copied().unwrap_or("")));
            block.push(end);
            append_block(&mut lines, block);
            changed.push(name.to_string());
        }
    }

    if changed.is_empty() {
        return unchanged(Vec::new());
    }
    changed.sort();
    Rendered {
        text: join(&lines, newline, bom),
        changed,
        errors: Vec::new(),
    }
}

/// İşaret bloğunu önünde tam bir boş satırla sona ekler.
fn append_block(lines: &mut Vec<String>, block: Vec<String>) {
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(block);
    lines.push(String::new());
}

/// Yönetilen blokları (işaretler + gövde) kaldırır; `/docs unmark`.
/// Sonuç: (yeni metin, kaldırılan adlar, hatalar).
pub fn strip_managed(text: &str) -> (String, Vec<String>, Vec<String>) {
    let (existing, errors) = parse_sections(text);
    if !errors.is_empty() {
        return (text.to_string(), Vec::new(), vec![MALFORMED.to_string()]);
    }
    if existing.is_empty() {
        return (text.to_string(), Vec::new(), Vec::new());
    }
    let Split {
        mut lines,
        newline,
        bom,
    } = split(text);
    let mut removed: Vec<String> = existing.iter().map(|s| s.name.clone()).collect();
    removed.sort();
    let mut spans: Vec<&SectionSpan> = existing.iter().collect();
    spans.sort_by_key(|s| std::cmp::Reverse(s.start_line));
    for span in spans {
        lines.drain(span.start_line..=span.end_line);
    }
    // Kaldırmaların bıraktığı ardışık boş satırlar teke indirilir.
    let mut cleaned: Vec<String> = Vec::new();
    for line in lines {
        if line.trim().is_empty() && cleaned.last().is_some_and(|l| l.trim().is_empty()) {
            continue;
        }
        cleaned.push(line);
    }
    while cleaned.last().is_some_and(|l| l.trim().is_empty()) {
        cleaned.pop();
    }
    if !cleaned.is_empty() {
        cleaned.push(String::new());
    }
    (join(&cleaned, newline, bom), removed, Vec::new())
}

#[cfg(test)]
mod tests {
    //! Python `tests/test_docs_merge.py`.
    use super::*;

    fn doc(lines: &[&str]) -> String {
        lines.join("\n") + "\n"
    }

    fn sm(name: &str) -> String {
        start_marker(name).unwrap_or_default()
    }

    fn em(name: &str) -> String {
        end_marker(name).unwrap_or_default()
    }

    fn one(name: &str, body: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(name.to_string(), body.to_string())])
    }

    #[test]
    fn markers_round_trip() {
        let block = wrap_section("runtime_env", "- os: Windows\n- shell: cmd").unwrap_or_default();
        assert!(block.contains(&sm("runtime_env")));
        assert!(block.contains(&em("runtime_env")));
        let (sections, errors) = parse_sections(&(block + "\n"));
        assert!(errors.is_empty());
        assert_eq!(sections[0].body_text(), "- os: Windows\n- shell: cmd");
        assert!(start_marker("Kötü Ad").is_none());
    }

    #[test]
    fn nested_start_is_malformed() {
        let text = doc(&[&sm("a"), "iç", &sm("b"), &em("b"), &em("a")]);
        let (sections, errors) = parse_sections(&text);
        assert!(sections.is_empty());
        assert!(!errors.is_empty());
    }

    #[test]
    fn malformed_document_is_untouchable() {
        let text = doc(&[&sm("a"), "iç"]);
        let r = render_sections(&text, &one("a", "x"), false, &[]);
        assert_eq!(r.text, text);
        assert!(r.changed.is_empty());
        assert_eq!(r.errors, vec![MALFORMED.to_string()]);
    }

    #[test]
    fn unmanaged_document_is_untouchable() {
        let text = doc(&["# elle"]);
        let r = render_sections(&text, &one("a", "x"), false, &[]);
        assert_eq!(r.text, text);
        assert_eq!(r.errors, vec![UNTOUCHABLE.to_string()]);
    }

    #[test]
    fn only_managed_body_changes() {
        let text = doc(&[
            "# agent.md",
            "",
            "## Elle bölüm",
            "korunacak metin",
            "",
            &sm("runtime_env"),
            "- eski: 1",
            &em("runtime_env"),
            "",
            "## Notlar (elle)",
            "alt not",
        ]);
        let r = render_sections(&text, &one("runtime_env", "- yeni: 2"), false, &[]);
        assert!(r.errors.is_empty());
        assert_eq!(r.changed, vec!["runtime_env".to_string()]);
        assert!(r.text.contains("korunacak metin"));
        assert!(r.text.contains("alt not"));
        assert!(r.text.contains("- yeni: 2"));
        assert!(!r.text.contains("- eski: 1"));
        // Aynı gövde ikinci kez değişiklik üretmez.
        let again = render_sections(&r.text, &one("runtime_env", "- yeni: 2"), false, &[]);
        assert!(again.changed.is_empty());
        assert_eq!(again.text, r.text);
    }

    #[test]
    fn crlf_and_bom_preserved() {
        let plain = doc(&["# başlık", &sm("a"), "eski", &em("a")]);
        let text = format!("\u{feff}{}", plain.replace('\n', "\r\n"));
        let r = render_sections(&text, &one("a", "yeni"), false, &[]);
        assert_eq!(r.changed, vec!["a".to_string()]);
        assert!(r.errors.is_empty());
        assert!(r.text.starts_with('\u{feff}'));
        assert!(r.text.contains("\r\nyeni\r\n"));
        assert_eq!(r.text.replace("\r\n", "").matches('\n').count(), 0);
    }

    #[test]
    fn adopt_appends_in_order() {
        let mut secs = BTreeMap::new();
        secs.insert("b".to_string(), "B".to_string());
        secs.insert("a".to_string(), "A".to_string());
        let r = render_sections("", &secs, true, &["b", "a"]);
        assert_eq!(
            r.text,
            format!(
                "{}\nB\n{}\n\n{}\nA\n{}\n",
                sm("b"),
                em("b"),
                sm("a"),
                em("a")
            )
        );
    }

    #[test]
    fn strip_keeps_human_text() {
        let text = doc(&[
            "# agent.md",
            "",
            "## Notlar",
            "elle metin",
            "",
            &sm("a"),
            "makine metni",
            &em("a"),
        ]);
        let (new, removed, errors) = strip_managed(&text);
        assert!(errors.is_empty());
        assert_eq!(removed, vec!["a".to_string()]);
        assert!(new.contains("elle metin"));
        assert!(!new.contains("makine metni"));
        assert!(!new.contains("evren:managed"));
    }

    #[test]
    fn hash_matches_python() {
        assert_eq!(section_hash("x"), section_hash("\n\nx\n"));
        assert!(section_hash("x").starts_with("sha256:"));
        assert_eq!(
            section_hash("abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
