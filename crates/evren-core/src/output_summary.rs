//! Uzun komut çıktıları için türe duyarlı özetler (Python `output_summary.py`).
//! Saf fonksiyonlar: test, grep benzeri liste, derleme ve log (Loki, kubectl logs,
//! heartbeat) çıktısını tanır;
//! tanınmayan çıktıda `None` döner ve çağıran baş/kuyruk kırpmasını kullanır.

use std::sync::OnceLock;

use regex::Regex;

const SUMMARY_MAX_CHARS: usize = 3000;
const BUILD_MAX_CHARS: usize = 3500;
const FAILURE_BLOCK_LINES: usize = 15;
const TAIL_LINES: usize = 20;
const BUILD_TAIL_LINES: usize = 10;
const GREP_SHOWN_LINES: usize = 80;
const GREP_MIN_LINES: usize = 100;
const GREP_FILE_COUNTS_SHOWN: usize = 40;
/// Bu kadar satırı geçen log çıktısı karakter sınırının altında da özetlenir.
pub const LOG_MIN_LINES: usize = 40;
const LOG_PATTERNS_SHOWN: usize = 15;
const LOG_PROBLEM_LINES: usize = 10;

struct Patterns {
    test_cmd: Regex,
    test_markers: Regex,
    build_cmd: Regex,
    build_markers: Regex,
    grep_cmd: Regex,
    grep_line: Regex,
    grep_line_nonum: Regex,
    file_of: Regex,
    fail_headers: Vec<Regex>,
    block_end: Regex,
    summary_line: Regex,
    fail_line: Regex,
    build_line: Regex,
    log_cmd: Regex,
    log_stamp: Regex,
    log_level: Regex,
    log_vary: Regex,
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|_| crate::tools::command::never_re())
}

fn pats() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        test_cmd: re(concat!(
            r"(?i)\b(pytest|py\.test|unittest|jest|vitest|mocha|phpunit|rspec)\b",
            r"|\bdotnet\s+test\b|\bnpm\s+(run\s+)?test\b|\bnpx\s+(jest|vitest)\b",
            r"|\bgo\s+test\b|\bcargo\s+test\b|\bpnpm\s+(run\s+)?test\b|\byarn\s+test\b"
        )),
        test_markers: re(concat!(
            r"(?m)^=+ .*\b(passed|failed|error|errors|no tests ran)\b.* =+\s*$",
            r"|^Ran \d+ tests? in ",
            r"|^(Passed|Failed)!\s+-\s+Failed:",
            r"|^test result: ",
            r"|^Tests:\s+.*\b(passed|failed)\b"
        )),
        build_cmd: re(concat!(
            r"(?i)\bdotnet\s+(build|publish|msbuild)\b|\bmsbuild\b|\bnpm\s+run\s+build\b",
            r"|\btsc\b|\bcargo\s+build\b|\bvite\s+build\b|\bpnpm\s+(run\s+)?build\b|\bgradlew?\s+build\b|\bmvn\b"
        )),
        build_markers: re(
            r"(?m)\b(error|warning)\s+(CS|TS|MSB|NU|CA|FS|BC)\d+\b|^Build (succeeded|FAILED)",
        ),
        grep_cmd: re(r"(?i)\b(grep|findstr|rg|ripgrep|ag|select-string)\b|\bgit\s+grep\b"),
        grep_line: re(r"^(?:[A-Za-z]:[\\/])?[^\s:][^:]*:\d+:"),
        grep_line_nonum: re(r"^(?:[A-Za-z]:[\\/])?[^\s:][^:]*:\S"),
        file_of: re(r"^((?:[A-Za-z]:[\\/])?[^:]+):"),
        fail_headers: vec![
            re(r"^_{3,} .+ _{3,}$"),
            re(r"^(FAIL|ERROR): \S"),
            re(r"^\s*Failed \S+"),
            re(r"^\s*● .+"),
            re(r"^--- FAIL: "),
            re(r"^---- \S+ stdout ----$"),
        ],
        block_end: re(r"^(={3,}|-{20,}|Ran \d+ tests?|Failed!|Passed!)"),
        summary_line: re(concat!(
            r"^=+ .*\b(passed|failed|error|errors|no tests ran|short test summary)\b.* =+\s*$",
            r"|^Ran \d+ tests? in ",
            r"|^(OK|FAILED)( \(.*\))?\s*$",
            r"|^(Passed|Failed)!",
            r"|^\s*(Total tests|Passed|Failed|Skipped|Total|Duration):",
            r"|^test result: ",
            r"|^Tests:\s|^Test Suites:|^Snapshots:|^\s*Time:",
            r"|^(ok|FAIL|PASS)\s+\S+\s",
            r"|^(FAILED|ERROR) \S"
        )),
        fail_line: re(r"^(FAILED|ERROR) \S"),
        build_line: re(concat!(
            r"(?i)\b(error|warning|hata|uyarı)\b|\bFAILED\b|^Build (succeeded|FAILED)",
            r"|\d+ (Warning|Error)\(s\)|Time Elapsed"
        )),
        log_cmd: re(concat!(
            r"(?i)\b(logcli|loki|journalctl|stern|heartbeat)\b|\bkubectl\s+(\S+\s+)*logs\b",
            r"|\bdocker\s+(compose\s+)?logs\b|query_range"
        )),
        log_stamp: re(concat!(
            r"^\s*(\[|\x22|ts=|time=)?(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}|\d{2}:\d{2}:\d{2}",
            r"|[A-Z][a-z]{2} +\d{1,2} \d{2}:\d{2}:\d{2}|\d{10,13}\b)"
        )),
        log_level: re(
            r"(?i)\b(?:level=)?\x22?(fatal|panic|crit(?:ical)?|error|err|warn(?:ing)?|info|debug|trace)\b",
        ),
        log_vary: re(concat!(
            r"\d{4}-\d{2}-\d{2}[T ][\d:.,]+(Z|[+-]\d{2}:?\d{2})?",
            r"|[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
            r"|\b[0-9a-f]{12,}\b|\d+(\.\d+)?"
        )),
    })
}

fn clen(s: &str) -> usize {
    s.chars().count()
}

fn take(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Python `str.splitlines()` (satır sonları: \n, \r\n, \r).
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            b'\r' => {
                out.push(&text[start..i]);
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn cap(text: &str, limit: usize) -> String {
    if clen(text) <= limit {
        return text.to_string();
    }
    format!(
        "{}\n... [özet kısaltıldı]",
        take(text, limit.saturating_sub(40)).trim_end()
    )
}

fn dedupe<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen = std::collections::HashSet::new();
    lines
        .into_iter()
        .filter(|l| seen.insert(l.trim()))
        .collect()
}

pub fn is_test_output(text: &str, command: &str) -> bool {
    let p = pats();
    p.test_cmd.is_match(command) || p.test_markers.is_match(text)
}

pub fn is_build_output(text: &str, command: &str) -> bool {
    let p = pats();
    p.build_cmd.is_match(command) || p.build_markers.is_match(text)
}

fn grep_lines<'a>(lines: &[&'a str], command: &str) -> Option<Vec<&'a str>> {
    let p = pats();
    let non_empty: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if non_empty.len() < GREP_MIN_LINES {
        return None;
    }
    let need = 0.7 * non_empty.len() as f64;
    let mut hits: Vec<&str> = non_empty
        .iter()
        .copied()
        .filter(|l| p.grep_line.is_match(l))
        .collect();
    if (hits.len() as f64) < need && p.grep_cmd.is_match(command) {
        hits = non_empty
            .iter()
            .copied()
            .filter(|l| p.grep_line_nonum.is_match(l))
            .collect();
    }
    ((hits.len() as f64) >= need).then_some(hits)
}

fn file_of(line: &str) -> String {
    match pats().file_of.captures(line).and_then(|c| c.get(1)) {
        Some(m) => m.as_str().to_string(),
        None => take(line, 60).to_string(),
    }
}

/// `collections.Counter(...).most_common()`: sayıya göre azalan, eşitlikte ilk görülen önce.
fn most_common(items: impl Iterator<Item = String>) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for item in items {
        match index.get(&item) {
            Some(&i) => counts[i].1 += 1,
            None => {
                index.insert(item.clone(), counts.len());
                counts.push((item, 1));
            }
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    counts
}

pub fn summarize_grep(hits: &[&str]) -> String {
    let shown = &hits[..hits.len().min(GREP_SHOWN_LINES)];
    let rest = &hits[shown.len()..];
    let files = most_common(hits.iter().map(|h| file_of(h))).len();
    let mut out: Vec<String> = shown.iter().map(|s| s.to_string()).collect();
    out.push(format!("... toplam {} eşleşme, {files} dosya", hits.len()));
    if !rest.is_empty() {
        let counts = most_common(rest.iter().map(|h| file_of(h)));
        let parts: Vec<String> = counts
            .iter()
            .take(GREP_FILE_COUNTS_SHOWN)
            .map(|(name, n)| format!("{name}: {n}"))
            .collect();
        let extra = counts.len() - parts.len();
        let tail = if extra > 0 {
            format!(" (+{extra} dosya daha)")
        } else {
            String::new()
        };
        out.push(format!(
            "Gösterilmeyen {} eşleşmenin dosya başına sayısı: {}{tail}",
            rest.len(),
            parts.join(", ")
        ));
    }
    out.join("\n")
}

fn is_header(line: &str) -> bool {
    pats().fail_headers.iter().any(|p| p.is_match(line))
}

pub fn summarize_tests(lines: &[&str]) -> String {
    let p = pats();
    let n = lines.len();
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut i = 0;
    while i < n {
        let line = lines[i];
        if is_header(line) && !p.block_end.is_match(line) {
            let mut block = vec![line];
            let mut j = i + 1;
            while j < n && block.len() < FAILURE_BLOCK_LINES {
                let next = lines[j];
                if p.block_end.is_match(next) || is_header(next) {
                    break;
                }
                block.push(next);
                j += 1;
            }
            blocks.push(block);
            while j < n && !(p.block_end.is_match(lines[j]) || is_header(lines[j])) {
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let summary: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| p.summary_line.is_match(l))
        .collect();
    let fail_lines: Vec<&str> = dedupe(summary.iter().copied().filter(|l| p.fail_line.is_match(l)))
        .into_iter()
        .take(30)
        .collect();
    let other: Vec<&str> = dedupe(summary.iter().copied().filter(|l| !p.fail_line.is_match(l)))
        .into_iter()
        .take(15)
        .collect();
    let tail_lines: Vec<&str> = lines[n.saturating_sub(TAIL_LINES)..]
        .iter()
        .map(|l| take(l, 200))
        .collect();
    let tail_text = format!("Son satırlar:\n{}", tail_lines.join("\n"));
    let mut head_parts: Vec<String> = Vec::new();
    if !other.is_empty() {
        head_parts.push(format!("Özet satırları:\n{}", other.join("\n")));
    }
    if !fail_lines.is_empty() {
        head_parts.push(fail_lines.join("\n"));
    }
    let fixed = clen(&head_parts.join("\n")) + clen(&tail_text) + 120;
    let budget = SUMMARY_MAX_CHARS.saturating_sub(fixed).max(600);
    let mut parts: Vec<String> = Vec::new();
    if !blocks.is_empty() {
        let per_block = (budget / blocks.len().max(1)).max(250);
        parts.push(format!("Başarısız test blokları ({}):", blocks.len()));
        let mut used = 0;
        let mut shown = 0;
        for block in &blocks {
            let text = cap(&block.join("\n"), per_block);
            if used + clen(&text) > budget && shown > 0 {
                break;
            }
            used += clen(&text);
            parts.push(text);
            shown += 1;
        }
        if shown < blocks.len() {
            parts.push(format!(
                "... {} başarısız blok daha (tam çıktıda)",
                blocks.len() - shown
            ));
        }
    }
    parts.extend(head_parts);
    parts.push(tail_text);
    parts.join("\n")
}

pub fn summarize_build(lines: &[&str]) -> String {
    let p = pats();
    let kept = dedupe(
        lines
            .iter()
            .filter(|l| !l.trim().is_empty() && p.build_line.is_match(l))
            .map(|l| l.trim()),
    );
    let tail = &lines[lines.len().saturating_sub(BUILD_TAIL_LINES)..];
    let tail_len: usize = tail.iter().map(|t| clen(t) + 1).sum();
    let budget = BUILD_MAX_CHARS as i64 - tail_len as i64 - 200;
    let mut shown: Vec<&str> = Vec::new();
    let mut used: i64 = 0;
    for ln in &kept {
        let add = clen(ln) as i64 + 1;
        if used + add > budget {
            break;
        }
        shown.push(ln);
        used += add;
    }
    let mut parts = vec![format!("Hata/uyarı satırları ({} benzersiz):", kept.len())];
    parts.extend(shown.iter().map(|s| s.to_string()));
    if shown.len() < kept.len() {
        parts.push(format!(
            "... {} satır daha (tam çıktıda)",
            kept.len() - shown.len()
        ));
    }
    parts.push("Son satırlar:".into());
    parts.extend(tail.iter().map(|s| s.to_string()));
    parts.join("\n")
}

/// Log çıktısı: komut log aracıysa ya da satırların yarısından çoğu zaman damgasıyla başlıyorsa.
pub fn is_log_output(text: &str, command: &str) -> bool {
    let p = pats();
    if p.log_cmd.is_match(command) {
        return true;
    }
    let lines: Vec<&str> = splitlines(text)
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .take(200)
        .collect();
    lines.len() >= 10 && lines.iter().filter(|l| p.log_stamp.is_match(l)).count() * 2 > lines.len()
}

fn level_of(line: &str) -> &'static str {
    match pats()
        .log_level
        .captures(line)
        .map(|c| c[1].to_ascii_lowercase())
        .as_deref()
    {
        Some("fatal" | "panic" | "crit" | "critical") => "FATAL",
        Some("error" | "err") => "ERROR",
        Some("warn" | "warning") => "WARN",
        Some("info") => "INFO",
        Some("debug" | "trace") => "DEBUG",
        _ => "-",
    }
}

/// Log özeti: seviye sayıları, zaman aralığı, tekrar eden kalıplar (sayı/kimlik/zaman `#`)
/// ve son hata/uyarı satırları.
pub fn summarize_logs(lines: &[&str]) -> String {
    let p = pats();
    let body: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let mut levels: Vec<(&str, usize)> = Vec::new();
    let mut patterns: Vec<(String, usize)> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut problems: Vec<&str> = Vec::new();
    for l in &body {
        let lv = level_of(l);
        match levels.iter_mut().find(|(k, _)| *k == lv) {
            Some((_, n)) => *n += 1,
            None => levels.push((lv, 1)),
        }
        if matches!(lv, "FATAL" | "ERROR" | "WARN") {
            problems.push(l);
        }
        let key = take(&p.log_vary.replace_all(l.trim(), "#"), 160).to_string();
        match index.get(&key) {
            Some(&i) => patterns[i].1 += 1,
            None => {
                index.insert(key.clone(), patterns.len());
                patterns.push((key, 1));
            }
        }
    }
    let order = ["FATAL", "ERROR", "WARN", "INFO", "DEBUG", "-"];
    levels.sort_by_key(|(k, _)| order.iter().position(|o| o == k));
    let lv: Vec<String> = levels.iter().map(|(k, n)| format!("{k}={n}")).collect();
    let mut out = vec![format!(
        "Log özeti: {} satır, {} farklı kalıp; seviyeler: {}",
        body.len(),
        patterns.len(),
        lv.join(" ")
    )];
    if let (Some(first), Some(last)) = (body.first(), body.last()) {
        out.push(format!("İlk: {}", take(first.trim(), 120)));
        out.push(format!("Son: {}", take(last.trim(), 120)));
    }
    let mut top: Vec<&(String, usize)> = patterns.iter().collect();
    top.sort_by_key(|e| std::cmp::Reverse(e.1));
    out.push(format!(
        "En sık {} kalıp:",
        top.len().min(LOG_PATTERNS_SHOWN)
    ));
    for (k, n) in top.iter().take(LOG_PATTERNS_SHOWN) {
        out.push(format!("  {n}× {k}"));
    }
    if !problems.is_empty() {
        let shown = dedupe(problems.iter().rev().copied());
        out.push(format!(
            "Son hata/uyarı satırları ({} toplam):",
            problems.len()
        ));
        for l in shown.iter().take(LOG_PROBLEM_LINES).rev() {
            out.push(format!("  {}", take(l.trim(), 300)));
        }
    }
    cap(&out.join("\n"), SUMMARY_MAX_CHARS)
}

/// Uzun çıktının özeti; tanınmayan çıktıda `None` (genel kırpma kalır).
pub fn summarize_output(text: &str, command: &str) -> Option<String> {
    let lines = splitlines(text);
    if lines.len() < 2 {
        return None;
    }
    let is_test = is_test_output(text, command);
    if let Some(hits) = grep_lines(&lines, command) {
        if !is_test {
            return Some(summarize_grep(&hits));
        }
    }
    if is_test {
        Some(summarize_tests(&lines))
    } else if is_log_output(text, command) {
        Some(summarize_logs(&lines))
    } else if is_build_output(text, command) {
        Some(summarize_build(&lines))
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn loki_logs_are_grouped() {
        let mut lines: Vec<String> = (0..50)
            .map(|i| {
                format!(
                    "2026-11-02T10:00:{:02}Z level=info msg=\"heartbeat ok\" seq={i}",
                    i % 60
                )
            })
            .collect();
        lines.push("2026-11-02T10:01:00Z level=error msg=\"db timeout\" id=42".into());
        let text = lines.join("\n");
        assert!(is_log_output(&text, "curl .../loki/api/v1/query_range"));
        assert!(is_log_output(&text, ""));
        let s = summarize_output(&text, "logcli query x").unwrap();
        assert!(s.contains("51 satır, 2 farklı kalıp"), "{s}");
        assert!(s.contains("ERROR=1 INFO=50"), "{s}");
        assert!(s.contains("50× # level=info"), "{s}");
        assert!(s.contains("db timeout"), "{s}");
    }

    #[test]
    fn grep_listing_is_summarized() {
        let text: String = (0..150)
            .map(|i| format!("src/f{}.py:{i}: eşleşme", i % 3))
            .collect::<Vec<_>>()
            .join("\n");
        let s = summarize_output(&text, "grep -rn x").unwrap();
        assert!(s.contains("... toplam 150 eşleşme, 3 dosya"));
        assert!(s.contains("Gösterilmeyen 70 eşleşmenin dosya başına sayısı: "));
    }

    #[test]
    fn pytest_failure_blocks() {
        let mut lines = vec!["===== test session starts =====".to_string()];
        lines.push("____ test_a ____".into());
        lines.extend((0..30).map(|i| format!("satır {i}")));
        lines.push("FAILED tests/test_x.py::test_a - assert".into());
        lines.push("===== 1 failed, 3 passed in 0.1s =====".into());
        let text = lines.join("\n");
        let s = summarize_output(&text, "").unwrap();
        assert!(s.starts_with("Başarısız test blokları (1):\n____ test_a ____"));
        assert!(s.contains("Özet satırları:\n===== 1 failed, 3 passed in 0.1s ====="));
        assert!(s.contains("FAILED tests/test_x.py::test_a - assert"));
        assert!(s.contains("Son satırlar:\n"));
    }

    #[test]
    fn build_and_unknown() {
        let text = "a\nerror CS1002: ; bekleniyor\nb\nBuild FAILED";
        let s = summarize_output(text, "").unwrap();
        assert!(s.starts_with("Hata/uyarı satırları (2 benzersiz):"));
        assert!(summarize_output("düz\nmetin", "ls").is_none());
        assert!(summarize_output("tek satır", "pytest").is_none());
        assert_eq!(splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
    }
}
