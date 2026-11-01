//! Dosya sistemi araçları (`tools.py`): view_file, edit_file, write_file,
//! list_directory, find_files, create_plan.

use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use super::diff::{apply_edits, count_changes, generate_unified_diff, ALREADY_APPLIED};
use super::file_ops::{
    is_safe_path, read_file_with_meta, resolve_path, resolve_workspace_path, safe_read_file,
    safe_write_file, split_lines, split_lines_keepends, write_changed_lines,
};
use super::{edit_request, plan_request, write_request, Proposal, ToolContext};

pub const VIEW_DEFAULT_LINES: usize = 400;
pub const VIEW_MAX_LINES: usize = 800;
pub const VIEW_MAX_CHARS: usize = 40_000;

pub const FIND_MAX_LINES: usize = 80;
pub const FIND_MAX_PER_FILE: usize = 12;
pub const FIND_LINE_CHARS: usize = 180;

/// Gezinti sırasında girilmeyen klasörler (`_JUNK_DIRS`).
pub const JUNK_DIRS: &[&str] = &[
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "__pycache__",
    ".evren",
    "bin",
    "obj",
    ".vs",
    ".idea",
    "TestResults",
    "coverage",
    ".next",
    ".nuget",
    ".wt",
    "packages",
];

const IMAGE_SUFFIXES: &[&str] = &[".png", ".jpg", ".jpeg", ".webp", ".gif"];

fn is_junk(name: &str) -> bool {
    JUNK_DIRS.contains(&name)
}

fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn posix_rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn rel_parts(path: &Path, root: &Path) -> Vec<String> {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

// ---------------------------------------------------------------- view_file

/// `tool_view_file`: numaralı satırlarla dosya görüntüler; aralık ve karakter sınırı uygular.
pub fn tool_view_file(
    ctx: &ToolContext,
    path: &str,
    start_line: Option<i64>,
    end_line: Option<i64>,
) -> String {
    let root = &ctx.root;
    let lower = path.to_lowercase();
    if IMAGE_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
        let resolved = resolve_workspace_path(path, root);
        if !resolved.exists() {
            return format!("HATA: Görsel bulunamadı: {path}");
        }
        return format!("HATA: Görsel açıklama bu sürümde desteklenmiyor: {path}");
    }
    let content = match safe_read_file(path, root, 0) {
        Ok(c) => c,
        Err(e) => return format!("HATA: {e}"),
    };
    let lines = split_lines(&content);
    let total = lines.len();
    let s = start_line.unwrap_or(1).max(1) as usize;
    let e_req = end_line
        .filter(|&e| e != 0)
        .map(|e| e.max(0) as usize)
        .unwrap_or(total);
    let mut e = e_req.min(total);
    if s > total {
        return format!("HATA: Başlangıç satırı ({s}) dosya satır sayısından ({total}) büyük.");
    }
    let limit = if start_line.is_none() && end_line.is_none() {
        VIEW_DEFAULT_LINES
    } else {
        VIEW_MAX_LINES
    };
    let mut clipped = false;
    if e >= s && e - s + 1 > limit {
        e = s + limit - 1;
        clipped = true;
    }
    let mut body = lines
        .iter()
        .enumerate()
        .skip(s - 1)
        .take(e.saturating_sub(s - 1))
        .map(|(i, line)| format!("{:4} | {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");
    if body.chars().count() > VIEW_MAX_CHARS {
        let cut = take_chars(&body, VIEW_MAX_CHARS);
        body = match cut.rfind('\n') {
            Some(i) => cut[..i].to_string(),
            None => cut.to_string(),
        };
        e = s + body.matches('\n').count();
        clipped = true;
    }
    let header = format!("--- {path} (Satırlar {s}-{e} / Toplam {total}) ---\n");
    let mut out = header + &body;
    if clipped {
        out.push_str(&format!(
            "\n... [kırpıldı: {}-{total} okunmadı. Gerekirse find_files(query=...) ile yeri bul ve view_file'ı start_line/end_line ile çağır]",
            e + 1
        ));
    }
    out
}

// ---------------------------------------------------------------- edit_file

fn probe_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(placeholder\w*|x|xx+|test|todo|dummy|foo|\.\.\.)\s*$")
            .unwrap_or_else(|_| fallback_re())
    })
}

fn fallback_re() -> Regex {
    #[allow(clippy::unwrap_used)]
    Regex::new(r"\A\z\Bx").unwrap()
}

/// `_diff_summary`: `+a/-r satır, ±d karakter; dosya n → m satır, geri kalanı değişmedi.`
pub fn diff_summary(old: &str, new: &str) -> String {
    let (added, removed) = count_changes(old, new);
    let delta = new.chars().count() as i64 - old.chars().count() as i64;
    let sign = if delta >= 0 { "+" } else { "" };
    let old_n = split_lines(old).len();
    let new_n = split_lines(new).len();
    format!("+{added}/-{removed} satır, {sign}{delta} karakter; dosya {old_n} → {new_n} satır, geri kalanı değişmedi.")
}

/// `tool_edit_file` parametreleri.
#[derive(Debug, Clone, Default)]
pub struct EditArgs<'a> {
    pub path: &'a str,
    pub target_snippet: &'a str,
    pub replacement_snippet: &'a str,
    pub all: bool,
    pub edits: Option<&'a [Value]>,
    pub start_line: Option<&'a Value>,
    pub end_line: Option<&'a Value>,
    pub auto_approve: bool,
}

fn value_to_int(v: &Value) -> Result<i64, ()> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or(()),
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| ()),
        Value::Bool(b) => Ok(i64::from(*b)),
        _ => Err(()),
    }
}

fn is_blank_value(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    }
}

/// `tool_edit_file`: snippet ya da satır aralığı ile düzenleme; diff özeti döner.
pub fn tool_edit_file(ctx: &ToolContext, args: EditArgs<'_>) -> String {
    let root = &ctx.root;
    let path = args.path;
    let (old, meta) = match read_file_with_meta(path, root, 0) {
        Ok(v) => v,
        Err(err) => {
            let resolved = resolve_workspace_path(path, root);
            if !resolved.exists() {
                return format!(
                    "HATA: '{path}' yok; edit_file yalnızca var olan dosyayı düzenler. Yeni dosya için write_file kullan."
                );
            }
            return format!("HATA: Dosya okunamadı: {err}");
        }
    };
    let edits = args.edits.filter(|e| !e.is_empty());
    let use_range = !is_blank_value(args.start_line) && edits.is_none();
    let new_content: String;
    let edit_count: usize;
    if use_range {
        let first = args.start_line.and_then(|v| value_to_int(v).ok());
        let last = match args.end_line {
            Some(v) if !is_blank_value(Some(v)) => value_to_int(v).ok(),
            _ => first,
        };
        let (Some(first), Some(last)) = (first, last) else {
            return "HATA: start_line/end_line tam sayı olmalı.".to_string();
        };
        let lines = split_lines_keepends(&old);
        let n = lines.len() as i64;
        if first < 1 || last < first || first > n {
            return format!("HATA: Geçersiz satır aralığı {first}-{last} (dosya {n} satır).");
        }
        let last = last.min(n) as usize;
        let first = first as usize;
        let eol = if old.contains("\r\n") { "\r\n" } else { "\n" };
        let mut block = args
            .replacement_snippet
            .replace("\r\n", "\n")
            .replace('\n', eol);
        if !block.is_empty() && !block.ends_with('\n') && lines[last - 1].ends_with('\n') {
            block.push_str(eol);
        }
        let head: String = lines[..first - 1].concat();
        let tail: String = lines[last..].concat();
        let candidate = format!("{head}{block}{tail}");
        if candidate == old {
            return format!("BİLGİ: '{path}' {first}-{last}. satırlar zaten bu içerikte.");
        }
        new_content = candidate;
        edit_count = 1;
    } else {
        let probes: Vec<String> = match edits {
            Some(list) => list
                .iter()
                .map(|e| {
                    e.get("target_snippet")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                })
                .collect(),
            None => vec![args.target_snippet.to_string()],
        };
        if probes.iter().any(|p| probe_re().is_match(p)) {
            return "HATA: target_snippet yer tutucu/deneme metni gibi görünüyor. Dosyayı görmek için view_file (start_line/end_line) kullan; edit_file'a yalnızca dosyadaki gerçek metni ver.".to_string();
        }
        let owned: Vec<Value>;
        let edit_list: &[Value] = match edits {
            Some(list) => list,
            None => {
                owned = vec![serde_json::json!({
                    "target_snippet": args.target_snippet,
                    "replacement_snippet": args.replacement_snippet,
                    "all": args.all,
                })];
                &owned
            }
        };
        edit_count = edit_list.len();
        new_content = match apply_edits(&old, edit_list) {
            Ok(s) => s,
            Err(rep_err) => {
                if rep_err.starts_with(ALREADY_APPLIED) {
                    return format!("BİLGİ: '{path}' {rep_err}");
                }
                return format!("HATA: Değişiklik uygulanamadı: {rep_err}");
            }
        };
    }
    let diff = generate_unified_diff(&old, &new_content, path);
    ctx.emit_info(&diff);
    let proposal = Proposal {
        path: path.to_string(),
        old: old.clone(),
        new: new_content.clone(),
    };
    let new_content =
        match ctx.request_approval_full(args.auto_approve, edit_request(path, Some(proposal))) {
            Err(denied) => return denied,
            Ok(Some(edited)) => edited,
            Ok(None) => new_content,
        };
    let backup = match write_changed_lines(path, &old, &new_content, root, Some(&meta)) {
        Ok(b) => b,
        Err((_, err)) => return format!("HATA: Dosya kaydedilemedi: {err}"),
    };
    let backup_info = backup
        .as_ref()
        .and_then(|p| {
            p.file_name()
                .map(|n| format!(" (Yedek: {})", n.to_string_lossy()))
        })
        .unwrap_or_default();
    let count_info = if edit_count > 1 {
        format!(" ({edit_count} düzenleme)")
    } else {
        String::new()
    };
    let summary = diff_summary(&old, &new_content);
    format!(
        "BAŞARILI: '{path}' dosyası başarıyla güncellendi{count_info}{backup_info}.\nDiff özeti: {summary}\nDoğrulama gerekmez: değişiklik diske yazıldı, dosyanın geri kalanı korundu; dosyayı yeniden okumana/view_file ile kontrol etmene gerek yok. Yalnızca davranışı etkileyen bir değişiklikse ilgili testi çalıştır."
    )
}

// ---------------------------------------------------------------- write_file

/// `tool_write_file`: dosyayı yazar ya da sonuna ekler.
pub fn tool_write_file(
    ctx: &ToolContext,
    path: &str,
    content: &str,
    auto_approve: bool,
    append: bool,
) -> String {
    let root = &ctx.root;
    let resolved = resolve_workspace_path(path, root);
    let exists = resolved.exists();
    let mut content = content.to_string();
    let mut old_text = String::new();
    if append && exists {
        if let Ok(old) = safe_read_file(path, root, 0) {
            let eol = if old.contains("\r\n") { "\r\n" } else { "\n" };
            let mut old = old;
            if !old.is_empty() && !old.ends_with('\n') {
                old.push_str(eol);
            }
            if eol == "\r\n" {
                content = content.replace("\r\n", "\n").replace('\n', "\r\n");
            }
            content = format!("{old}{content}");
        }
    }
    if exists {
        if old_text.is_empty() {
            old_text = safe_read_file(path, root, 0).unwrap_or_default();
        }
        ctx.emit_info(&generate_unified_diff(
            &old_text,
            &content,
            &format!("{path} (Üzerine Yazılacak)"),
        ));
    } else {
        ctx.emit_info(&format!(
            "Yeni dosya oluşturuluyor: {path} ({} karakter)",
            content.chars().count()
        ));
    }
    let action = if exists {
        "üzerine yazılsın"
    } else {
        "oluşturulsun"
    };
    let proposal = Proposal {
        path: path.to_string(),
        old: old_text,
        new: content.clone(),
    };
    let content = match ctx
        .request_approval_full(auto_approve, write_request(path, action, Some(proposal)))
    {
        Err(denied) => return denied,
        Ok(Some(edited)) => edited,
        Ok(None) => content,
    };
    let backup = match safe_write_file(path, &content, root, None, None) {
        Ok(b) => b,
        Err((_, err)) => return format!("HATA: Dosya yazılamadı: {err}"),
    };
    let backup_info = backup
        .as_ref()
        .and_then(|p| {
            p.file_name()
                .map(|n| format!(" (Eski hali yedeklendi: {})", n.to_string_lossy()))
        })
        .unwrap_or_default();
    format!("BAŞARILI: '{path}' başarıyla kaydedildi{backup_info}.")
}

// ---------------------------------------------------------------- list_directory

fn sorted_entries(dir: &Path) -> Vec<std::fs::DirEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    entries
}

fn is_dir_no_follow(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
}

fn list_recursive(start: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![start.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in sorted_entries(&current) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_junk(&name) {
                continue;
            }
            let path = entry.path();
            out.push(path.clone());
            if is_dir_no_follow(&entry) {
                stack.push(path);
            }
        }
    }
    out
}

/// `tool_list_directory`: 📁/📄 satırları, en fazla 100 kayıt.
pub fn tool_list_directory(ctx: &ToolContext, path: &str, recursive: bool) -> String {
    let root = &ctx.root;
    let path = if path.is_empty() { "." } else { path };
    let resolved = resolve_workspace_path(path, root);
    let (safe, reason) = is_safe_path(&resolved, root, true);
    if !safe {
        return format!("HATA: {reason}");
    }
    if !resolved.exists() {
        return format!("HATA: Klasör bulunamadı: {}", resolved.display());
    }
    if !resolved.is_dir() {
        return format!(
            "HATA: Belirtilen yol bir klasör değil: {}",
            resolved.display()
        );
    }
    let items: Vec<PathBuf> = if recursive {
        list_recursive(&resolved)
    } else {
        std::fs::read_dir(&resolved)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    let skip = [".git", ".venv", "node_modules", "__pycache__", ".evren"];
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        if rel_parts(&item, root)
            .iter()
            .any(|p| skip.contains(&p.as_str()))
        {
            continue;
        }
        let rel = posix_rel(&item, root);
        let kind = if item.is_dir() {
            "📁 DIR "
        } else {
            "📄 FILE"
        };
        lines.push(format!("{kind}  {rel}"));
    }
    lines.sort();
    lines.truncate(100);
    if lines.is_empty() {
        "Klasör boş.".to_string()
    } else {
        lines.join("\n")
    }
}

// ---------------------------------------------------------------- find_files

/// Python `fnmatch` deseni → düzenli ifade (Windows'ta büyük/küçük harf duyarsız).
pub fn fnmatch_regex(pat: &str) -> Option<Regex> {
    let mut re = String::from("^");
    let chars: Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '[' => {
                let mut j = i + 1;
                if j < chars.len() && chars[j] == '!' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j >= chars.len() {
                    re.push_str("\\[");
                } else {
                    let inner: String = chars[i + 1..j].iter().collect();
                    let inner = inner.replace('\\', "\\\\");
                    let inner = if let Some(rest) = inner.strip_prefix('!') {
                        format!("^{rest}")
                    } else if let Some(rest) = inner.strip_prefix('^') {
                        format!("\\^{rest}")
                    } else {
                        inner
                    };
                    re.push('[');
                    re.push_str(&inner);
                    re.push(']');
                    i = j;
                }
            }
            _ => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    let flags = if cfg!(windows) { "(?is)" } else { "(?s)" };
    Regex::new(&format!("{flags}{re}")).ok()
}

/// `fnmatch.fnmatch(name, pat)`.
pub fn fnmatch(name: &str, pat: &str) -> bool {
    fnmatch_regex(pat)
        .map(|r| r.is_match(name))
        .unwrap_or(false)
}

/// `PurePath(rel).match(pat)`: sağdan hizalı bileşen eşleşmesi.
fn purepath_match(rel: &str, pat: &str) -> bool {
    let pat_parts: Vec<&str> = pat.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    let rel_parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    if pat_parts.is_empty() || pat_parts.len() > rel_parts.len() {
        return false;
    }
    let offset = rel_parts.len() - pat_parts.len();
    pat_parts
        .iter()
        .enumerate()
        .all(|(i, p)| fnmatch(rel_parts[offset + i], p))
}

/// `_pattern_matcher`: `rglob(pattern)` benzeri eşleştirici.
pub fn pattern_matches(root: &Path, path: &Path, pattern: &str) -> bool {
    let pat = if pattern.is_empty() { "*" } else { pattern };
    let pat = pat.strip_prefix("**/").unwrap_or(pat);
    if !pat.contains('/') && !pat.contains('\\') {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return fnmatch(&name, pat);
    }
    purepath_match(&posix_rel(path, root), pat)
}

fn git_files(repo: &Path) -> Option<Vec<PathBuf>> {
    let out = Command::new("git")
        .args([
            "-C",
            &repo.to_string_lossy(),
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.split('\0')
            .filter(|s| !s.is_empty())
            .map(|rel| repo.join(rel))
            .collect(),
    )
}

/// `iter_workspace_files`: sıralı dosya listesi; git depolarında `git ls-files`.
pub fn iter_workspace_files(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        if current.join(".git").exists() {
            if let Some(files) = git_files(&current) {
                for f in files {
                    let parts = rel_parts(&f, root);
                    let parent_junk = parts
                        .iter()
                        .take(parts.len().saturating_sub(1))
                        .any(|p| is_junk(p));
                    if f.is_file() && !parent_junk && pattern_matches(root, &f, pattern) {
                        found.insert(f);
                    }
                }
                continue;
            }
        }
        let Ok(rd) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if !is_junk(&name) {
                    stack.push(entry.path());
                }
            } else if ft.is_file() {
                let f = entry.path();
                if pattern_matches(root, &f, pattern) {
                    found.insert(f);
                }
            }
        }
    }
    found.into_iter().collect()
}

fn regex_hint_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\\[.dswbDSWB()\[\]{}+*?^$]|\(\?|[.*+?]\*|\[[^\]]+\]|\([^()]+\|[^()]+\)|\^|\$$")
            .unwrap_or_else(|_| fallback_re())
    })
}

/// `_query_matcher`: düzenli ifade gibi görünüyorsa regex, değilse `|` ayrımlı literal.
pub enum QueryMatcher {
    Regex(Regex),
    Needles(Vec<String>),
}

impl QueryMatcher {
    pub fn new(query: &str, needles: &[String]) -> Self {
        if !query.is_empty() && regex_hint_re().is_match(query) {
            if let Ok(re) = Regex::new(&format!("(?i){query}")) {
                return QueryMatcher::Regex(re);
            }
        }
        QueryMatcher::Needles(needles.to_vec())
    }

    pub fn is_match(&self, line: &str) -> bool {
        match self {
            QueryMatcher::Regex(re) => re.is_match(line),
            QueryMatcher::Needles(needles) => {
                let folded = line.to_lowercase();
                needles.iter().any(|n| folded.contains(n.as_str()))
            }
        }
    }
}

fn git_ignored(root: &Path, paths: &[PathBuf]) -> HashSet<String> {
    let mut set = HashSet::new();
    if paths.is_empty() || !root.join(".git").exists() {
        return set;
    }
    let rels: Vec<String> = paths.iter().map(|p| posix_rel(p, root)).collect();
    let input = rels.join("\0") + "\0";
    let child = Command::new("git")
        .args(["check-ignore", "--stdin", "-z"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return set;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return set;
    };
    for x in String::from_utf8_lossy(&out.stdout).split('\0') {
        if !x.is_empty() {
            set.insert(x.to_string());
        }
    }
    set
}

fn looks_binary(path: &Path) -> bool {
    let Ok(mut fh) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 1024];
    let n = fh.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

fn clip_line(line: &str) -> String {
    take_chars(line.trim_end(), FIND_LINE_CHARS).to_string()
}

fn context_blocks(rel: &str, lines: &[&str], matcher: &QueryMatcher, context: i64) -> Vec<String> {
    let context = context.clamp(1, 15) as usize;
    let hit_idx: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| matcher.is_match(l))
        .map(|(i, _)| i)
        .take(FIND_MAX_PER_FILE)
        .collect();
    if hit_idx.is_empty() {
        return Vec::new();
    }
    let hits: HashSet<usize> = hit_idx.iter().copied().collect();
    let mut out = Vec::new();
    let mut last: i64 = -1;
    for &i in &hit_idx {
        let mut start = i.saturating_sub(context);
        let end = (i + context + 1).min(lines.len());
        if (start as i64) <= last {
            start = (last + 1) as usize;
        } else if !out.is_empty() {
            out.push("--".to_string());
        }
        for (j, line) in lines.iter().enumerate().take(end).skip(start) {
            let sep = if hits.contains(&j) { ":" } else { "-" };
            out.push(format!("{rel}{sep}{}{sep} {}", j + 1, clip_line(line)));
        }
        last = last.max(end as i64 - 1);
    }
    out
}

/// `tool_find_files`: glob ile dosya bulur; `query` ile `grep -n` biçiminde satırlar.
pub fn tool_find_files(ctx: &ToolContext, pattern: &str, query: &str, context: i64) -> String {
    let root = resolve_path(&ctx.root);
    let skip = [
        ".git",
        ".venv",
        "venv",
        "node_modules",
        "__pycache__",
        ".evren",
    ];
    let needles: Vec<String> = query
        .split('|')
        .filter(|q| !q.trim().is_empty())
        .map(|q| q.to_lowercase())
        .collect();
    let matcher = QueryMatcher::new(query, &needles);
    let mut files: Vec<String> = Vec::new();
    let mut lines_out: Vec<String> = Vec::new();
    let mut truncated = false;
    let pattern = if pattern.is_empty() { "*" } else { pattern };
    let candidates: Vec<PathBuf> = iter_workspace_files(&root, pattern)
        .into_iter()
        .filter(|p| {
            !rel_parts(p, &root)
                .iter()
                .any(|part| skip.contains(&part.as_str()))
        })
        .collect();
    let ignored = git_ignored(&root, &candidates);
    for path in &candidates {
        let rel = posix_rel(path, &root);
        if ignored.contains(&rel) || looks_binary(path) {
            continue;
        }
        if needles.is_empty() {
            files.push(rel);
            if files.len() >= 100 {
                files.push("... [100 dosya sınırına ulaşıldı]".to_string());
                break;
            }
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let text_lines = split_lines(&text);
        if context != 0 {
            let block = context_blocks(&rel, &text_lines, &matcher, context);
            if !block.is_empty() {
                lines_out.extend(block);
                if lines_out.len() >= FIND_MAX_LINES * 3 {
                    lines_out.push("... [satır sınırı; aramayı daralt]".to_string());
                    break;
                }
            }
            continue;
        }
        let mut hits = 0;
        for (idx, line) in text_lines.iter().enumerate() {
            if !matcher.is_match(line) {
                continue;
            }
            hits += 1;
            if hits > FIND_MAX_PER_FILE {
                lines_out.push(format!("{rel}: ... [bu dosyada daha fazla eşleşme var]"));
                break;
            }
            lines_out.push(format!("{rel}:{}: {}", idx + 1, clip_line(line)));
            if lines_out.len() >= FIND_MAX_LINES {
                truncated = true;
                break;
            }
        }
        if truncated {
            lines_out.push(format!(
                "... [{FIND_MAX_LINES} satır sınırı; aramayı daralt]"
            ));
            break;
        }
    }
    if !needles.is_empty() {
        return if lines_out.is_empty() {
            "Eşleşme bulunamadı.".to_string()
        } else {
            lines_out.join("\n")
        };
    }
    if files.is_empty() {
        "Eşleşen dosya bulunamadı.".to_string()
    } else {
        files.join("\n")
    }
}

// ---------------------------------------------------------------- create_plan

/// `tool_create_plan`: `plans/<ad>/plan.md` dosyasına yazar.
pub fn tool_create_plan(
    ctx: &ToolContext,
    name: &str,
    content: &str,
    auto_approve: bool,
) -> String {
    let root = &ctx.root;
    let safe_name: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe_name.is_empty() {
        return "HATA: Geçersiz plan adı.".to_string();
    }
    let rel = format!("plans/{safe_name}/plan.md");
    let resolved = resolve_workspace_path(&rel, root);
    let (safe, reason) = is_safe_path(&resolved, root, false);
    if !safe {
        return format!("HATA: {reason}");
    }
    if let Some(denied) = ctx.request_approval(auto_approve, plan_request(&rel)) {
        return denied;
    }
    if let Err((_, err)) = safe_write_file(&rel, content, root, None, None) {
        return format!("HATA: Plan kaydedilemedi: {err}");
    }
    format!("BAŞARILI: Plan '{rel}' dosyasına kaydedildi.")
}

/// `_syntax_report`: JSON dosyası bozuksa uyarı ekler (Python `py_compile` kapsam dışı).
pub fn syntax_report(path: &str, root: &Path) -> String {
    if !path.to_lowercase().ends_with(".json") {
        return String::new();
    }
    let resolved = resolve_workspace_path(path, root);
    let Ok(text) = std::fs::read_to_string(&resolved) else {
        return String::new();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(_) => String::new(),
        Err(e) => format!("\n⚠ JSON HATASI: {path}: {e}. Bir sonraki adımda önce bunu düzelt."),
    }
}
