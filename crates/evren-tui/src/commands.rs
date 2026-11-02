//! Slash komut tablosu ve girdi sınıflandırıcısı (Python `commands.py`).
//!
//! Bilinen komut ya da yazım hatası komut işleyiciye gider; yapıştırılan yol,
//! derleme çıktısı ve `/` ile başlayan çok satırlı metin görev metnidir.

use std::collections::{BTreeSet, HashMap};
use std::sync::{OnceLock, RwLock};

use regex::Regex;

/// Kanonik komut ve takma adları.
pub const COMMAND_ALIASES: &[(&str, &[&str])] = &[
    ("/exit", &["/quit", "/q"]),
    ("/help", &["/h", "/?"]),
    ("/version", &["/ver", "/v"]),
    ("/model", &[]),
    ("/mode", &[]),
    ("/effort", &[]),
    ("/models", &[]),
    ("/quota", &["/status"]),
    ("/terms", &[]),
    ("/keys", &[]),
    ("/tokens", &[]),
    ("/steps", &[]),
    ("/docs", &[]),
    ("/debug", &[]),
    ("/skills", &[]),
    ("/skill", &["/apply"]),
    ("/diagnose", &[]),
    ("/template", &[]),
    ("/memory", &[]),
    ("/track", &[]),
    ("/task", &[]),
    ("/progress", &[]),
    ("/playbook", &[]),
    ("/resume", &[]),
    ("/language", &[]),
    ("/scan", &[]),
    ("/files", &[]),
    ("/add", &[]),
    ("/drop", &[]),
    ("/undo", &["/rollback"]),
    ("/auto", &[]),
    ("/run", &[]),
    ("/bg", &[]),
    ("/todos", &[]),
    ("/ssh", &[]),
    ("/update", &[]),
    ("/clear", &[]),
    ("/continue", &[]),
    ("/execute", &[]),
    ("/checkpoint", &[]),
    ("/blockers", &[]),
    ("/check-in", &[]),
    ("/decisions", &[]),
    ("/commit", &[]),
    ("/review", &[]),
    ("/init", &[]),
    ("/context", &[]),
    ("/compact", &[]),
    ("/rewind", &[]),
    ("/doctor", &[]),
    ("/rol", &[]),
    ("/agent", &[]),
    ("/agents", &[]),
    ("/team", &[]),
    ("/iterate", &["/iterasyon"]),
];

/// Argümanlı girdide yazım hatası sayılma eşiği.
const NEAR_MISS_CUTOFF: f64 = 0.8;

/// Kanonik adlar ve takma adlar, sıralı.
pub fn known_commands() -> &'static BTreeSet<&'static str> {
    static KNOWN: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        COMMAND_ALIASES
            .iter()
            .flat_map(|(c, aliases)| std::iter::once(*c).chain(aliases.iter().copied()))
            .collect()
    })
}

/// Takma adı kanonik ada çevirir (bilinmeyen ad küçük harfle döner).
pub fn canonical(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    for (c, aliases) in COMMAND_ALIASES {
        if *c == lower || aliases.contains(&lower.as_str()) {
            return (*c).to_string();
        }
    }
    lower
}

fn command_like_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^/(?:[^\W\d_]|\?)[\w-]*$").ok())
        .as_ref()
}

/// İlk boşlukla ayrılmış sözcük (boş metinde "").
pub fn first_token(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or("")
}

/// `.evren/commands/*.md` özel komutları (çalışma anında kaydedilir).
fn dynamic_commands() -> &'static RwLock<BTreeSet<String>> {
    static DYNAMIC: OnceLock<RwLock<BTreeSet<String>>> = OnceLock::new();
    DYNAMIC.get_or_init(|| RwLock::new(BTreeSet::new()))
}

pub fn set_dynamic_commands<I, S>(names: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if let Ok(mut set) = dynamic_commands().write() {
        set.clear();
        set.extend(names.into_iter().map(|n| n.as_ref().to_lowercase()));
    }
}

fn is_dynamic(token: &str) -> bool {
    dynamic_commands()
        .read()
        .is_ok_and(|set| set.contains(token))
}

/// İlk sözcük bilinen ya da özel komut mu.
pub fn is_command(text: &str) -> bool {
    let token = first_token(text).to_lowercase();
    known_commands().contains(token.as_str()) || is_dynamic(&token)
}

/// `/word ...` biçiminde, yanlış yazılmış komut gibi duran tek satırlık girdi.
pub fn is_unknown_command(text: &str) -> bool {
    let stripped = text.trim();
    if stripped.is_empty() || stripped.contains(['\n', '\r']) {
        return false;
    }
    let token = first_token(stripped);
    let command_like = command_like_re().is_some_and(|re| re.is_match(token));
    let lower = token.to_lowercase();
    if known_commands().contains(lower.as_str()) || is_dynamic(&lower) || !command_like {
        return false;
    }
    stripped == token || suggest_command_with(token, NEAR_MISS_CUTOFF).is_some()
}

/// Girdi ajana değil komut işleyiciye mi gider.
pub fn is_command_input(text: &str) -> bool {
    let trimmed = text.trim();
    let (first, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
    if matches!(first.to_lowercase().as_str(), "/ask" | "/plan") && !rest.trim().is_empty() {
        return false;
    }
    is_command(text) || is_unknown_command(text)
}

/// Yanlış yazılmış sözcüğe en yakın komut (`/cleare` -> `/clear`).
pub fn suggest_command(token: &str) -> Option<String> {
    suggest_command_with(token, 0.6)
}

/// Aynı harflerden oluşan aday (`/modle` -> `/model`) daha yüksek puanlı
/// kısa sözcüğün (`/mode`) önüne geçer.
fn suggest_command_with(token: &str, cutoff: f64) -> Option<String> {
    let tok = token.to_lowercase();
    let candidates: Vec<&str> = known_commands().iter().copied().collect();
    let matches = get_close_matches(&tok, &candidates, 4, cutoff);
    let mut sorted_tok: Vec<char> = tok.chars().collect();
    sorted_tok.sort_unstable();
    for cand in &matches {
        let mut chars: Vec<char> = cand.chars().collect();
        chars.sort_unstable();
        if chars == sorted_tok {
            return Some((*cand).to_string());
        }
    }
    matches.first().map(|m| (*m).to_string())
}

/// Bilinmeyen komut uyarısı, varsa öneriyle.
pub fn unknown_command_message(cmd: &str) -> String {
    let hint = suggest_command(cmd)
        .map(|h| format!(" Bunu mu demek istediniz: {h}?"))
        .unwrap_or_default();
    format!("Bilinmeyen komut: '{cmd}'.{hint} Komut listesi için /help yazın.")
}

/// `difflib.get_close_matches`: puana göre azalan, eşitlikte büyük metin önce.
fn get_close_matches<'a>(
    word: &str,
    possibilities: &[&'a str],
    n: usize,
    cutoff: f64,
) -> Vec<&'a str> {
    let b: Vec<char> = word.chars().collect();
    let mut scored: Vec<(f64, &'a str)> = possibilities
        .iter()
        .filter_map(|x| {
            let a: Vec<char> = x.chars().collect();
            let r = ratio(&a, &b);
            (r >= cutoff).then_some((r, *x))
        })
        .collect();
    scored.sort_by(|l, r| r.0.total_cmp(&l.0).then_with(|| r.1.cmp(l.1)));
    scored.into_iter().take(n).map(|(_, x)| x).collect()
}

/// `SequenceMatcher(None, a, b).ratio()` (kısa metin; autojunk devrede değil).
fn ratio(a: &[char], b: &[char]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, c) in b.iter().enumerate() {
        b2j.entry(*c).or_default().push(j);
    }
    let mut matched = 0;
    let mut queue = vec![(0, a.len(), 0, b.len())];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, &b2j, (alo, ahi), (blo, bhi));
        if k > 0 {
            matched += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    2.0 * matched as f64 / total as f64
}

/// `find_longest_match`: en uzun ortak blok, eşitlikte en erken konum.
fn longest_match(
    a: &[char],
    b2j: &HashMap<char, Vec<usize>>,
    (alo, ahi): (usize, usize),
    (blo, bhi): (usize, usize),
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, c) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut newj2len = HashMap::new();
        for &j in b2j.get(c).map(Vec::as_slice).unwrap_or(&[]) {
            if j < blo {
                continue;
            }
            if j >= bhi {
                break;
            }
            let prev = j
                .checked_sub(1)
                .and_then(|p| j2len.get(&p))
                .copied()
                .unwrap_or(0);
            let k = prev + 1;
            newj2len.insert(j, k);
            if k > bestsize {
                besti = i + 1 - k;
                bestj = j + 1 - k;
                bestsize = k;
            }
        }
        j2len = newj2len;
    }
    (besti, bestj, bestsize)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD_ERROR: &str =
        "/src/Portal.Api/A.cs(12,5): error CS0103: The name 'x' does not exist";

    fn r(a: &str, b: &str) -> f64 {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        ratio(&a, &b)
    }

    #[test]
    fn difflib_ratio_matches_python() {
        assert!((r("/clear", "/cleare") - 12.0 / 13.0).abs() < 1e-9);
        assert!((r("abcd", "bcde") - 0.75).abs() < 1e-9);
        assert!((r("/help", "/hlep") - 0.8).abs() < 1e-9);
        assert!((r("", "") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn path_like_and_non_slash_text_is_task_text() {
        for text in [
            BUILD_ERROR,
            "/home/a",
            "/usr/bin/x: command not found",
            "/c:/work/x.cs(1,1): error CS1002",
            "//comment style line",
            "/home is full, free some space",
            "/src klasörünü incele",
            "",
            "   ",
            "clear",
            "devam et",
            "ask: /clear",
            "/cleare\nikinci satır",
            "/foo\n/bar.cs(1,1): error CS1002",
            "/ask bu dosya ne yapıyor",
            "/plan yeni modül",
        ] {
            assert!(!is_command_input(text), "{text}");
        }
        assert!(is_command_input("/exti now"));
    }

    #[test]
    fn known_commands_stay_commands() {
        for text in [
            "/clear",
            "/ssh list",
            "/help",
            "/exit",
            "/q",
            "/?",
            "/CLEAR",
            "/Model qwen",
            "/run echo hi",
            "/continue",
            "/ssh add a u@h 22",
            "/run echo a\necho b",
        ] {
            assert!(is_command(text), "{text}");
            assert!(is_command_input(text), "{text}");
        }
        for text in ["/cleare", "/hlep", "/zzzzzz"] {
            assert!(!is_command(text), "{text}");
            assert!(is_unknown_command(text), "{text}");
            assert!(is_command_input(text), "{text}");
        }
    }

    #[test]
    fn suggestion_and_first_token() {
        assert_eq!(suggest_command("/cleare").as_deref(), Some("/clear"));
        assert_eq!(suggest_command("/hlep").as_deref(), Some("/help"));
        assert_eq!(suggest_command("/modle").as_deref(), Some("/model"));
        assert_eq!(suggest_command("/zzzzzz"), None);
        let message = unknown_command_message("/cleare");
        assert!(
            message.contains("Bunu mu demek istediniz: /clear?"),
            "{message}"
        );
        assert!(message.contains("Komut listesi için /help"));
        assert!(!unknown_command_message("/zzzzzz").contains("Bunu mu"));
        assert_eq!(first_token("  /ssh   list "), "/ssh");
        assert_eq!(first_token(""), "");
    }

    #[test]
    fn aliases_resolve_to_canonical_names() {
        assert_eq!(canonical("/Q"), "/exit");
        assert_eq!(canonical("/status"), "/quota");
        assert_eq!(canonical("/rollback"), "/undo");
        assert_eq!(canonical("/iterasyon"), "/iterate");
        assert_eq!(canonical("/yok"), "/yok");
    }
}
