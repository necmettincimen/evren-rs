//! `apply_patch` aracı (Python `apply_patch.py`): çok dosyalı unified diff.

use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use super::file_ops::{backup_file, is_safe_path, resolve_workspace_path};
use super::{ApprovalKind, ApprovalRequest, ToolContext};

/// Tek yama parçası.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub start: usize,
    pub lines: Vec<String>,
}

/// Yamadaki bir dosya girdisi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFile {
    pub old: Option<String>,
    pub new: Option<String>,
    pub hunks: Vec<Hunk>,
}

fn hunk_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap_or_else(|_| {
            #[allow(clippy::unwrap_used)]
            Regex::new(r"\A\z\Bx").unwrap()
        })
    })
}

fn strip_ab(p: &str) -> Option<String> {
    if p == "/dev/null" {
        return None;
    }
    let s = p
        .strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p);
    Some(s.to_string())
}

fn header_path(line: &str) -> String {
    line[4..]
        .split('\t')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// `parse_patch`: `---`/`+++` çiftleri ve `@@` parçaları.
pub fn parse_patch(text: &str) -> Vec<PatchFile> {
    let normalized = text.replace("\r\n", "\n");
    let trimmed = normalized.trim_end_matches('\n');
    let lines: Vec<&str> = trimmed.split('\n').collect();
    let mut files: Vec<PatchFile> = Vec::new();
    let mut has_hunk = false;
    let mut i = 0;
    while i < lines.len() {
        let ln = lines[i];
        if ln.starts_with("--- ") && i + 1 < lines.len() && lines[i + 1].starts_with("+++ ") {
            let old = header_path(ln);
            let new = header_path(lines[i + 1]);
            files.push(PatchFile {
                old: strip_ab(&old),
                new: strip_ab(&new),
                hunks: Vec::new(),
            });
            has_hunk = false;
            i += 2;
            continue;
        }
        if let (Some(m), Some(cur)) = (hunk_re().captures(ln), files.last_mut()) {
            let start = m.get(1).and_then(|g| g.as_str().parse().ok()).unwrap_or(0);
            cur.hunks.push(Hunk {
                start,
                lines: Vec::new(),
            });
            has_hunk = true;
        } else if has_hunk {
            if let Some(cur) = files.last_mut() {
                if let Some(h) = cur.hunks.last_mut() {
                    if ln.starts_with([' ', '+', '-']) {
                        h.lines.push(ln.to_string());
                    } else if ln.is_empty() {
                        h.lines.push(" ".to_string());
                    }
                }
            }
        }
        i += 1;
    }
    files
}

/// `_apply`: parçaları sırayla uygular; bağlam uyuşmazsa hata.
pub fn apply_hunks(orig: &[String], hunks: &[Hunk]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = orig.to_vec();
    let mut pos = 0usize;
    for h in hunks {
        let old: Vec<&str> = h
            .lines
            .iter()
            .filter(|l| l.starts_with([' ', '-']))
            .map(|l| &l[1..])
            .collect();
        let new: Vec<String> = h
            .lines
            .iter()
            .filter(|l| l.starts_with([' ', '+']))
            .map(|l| l[1..].to_string())
            .collect();
        let guess = h.start.saturating_sub(1);
        let eq_at = |cand: usize, strict: bool| -> bool {
            if cand + old.len() > out.len() {
                return false;
            }
            out[cand..cand + old.len()]
                .iter()
                .zip(old.iter())
                .all(|(a, b)| {
                    if strict {
                        a == b
                    } else {
                        a.trim_end() == b.trim_end()
                    }
                })
        };
        let mut idx = None;
        let upper = out.len().saturating_sub(old.len());
        let candidates = std::iter::once(guess).chain(pos..=upper);
        for cand in candidates {
            if cand >= pos && eq_at(cand, true) {
                idx = Some(cand);
                break;
            }
        }
        if idx.is_none() {
            for j in 0..=upper {
                if eq_at(j, false) {
                    idx = Some(j);
                    break;
                }
            }
        }
        let Some(idx) = idx else {
            return Err(format!(
                "hunk @@ -{} uygulanamadı (bağlam uyuşmuyor)",
                h.start
            ));
        };
        let new_len = new.len();
        out.splice(idx..idx + old.len(), new);
        pos = idx + new_len;
    }
    Ok(out)
}

fn cut(patch: &str, n: usize) -> &str {
    match patch.char_indices().nth(n) {
        Some((i, _)) => &patch[..i],
        None => patch,
    }
}

/// `tool_apply_patch`: önizleme, onay, yedek ve yazma.
pub fn tool_apply_patch(ctx: &ToolContext, patch: &str, auto_approve: bool) -> String {
    let root = &ctx.root;
    let files = parse_patch(patch);
    if files.is_empty() {
        return "HATA: Geçerli unified diff bulunamadı.".to_string();
    }
    struct Planned {
        target: PathBuf,
        rel: String,
        text: Option<String>,
        existed: bool,
    }
    let mut plan: Vec<Planned> = Vec::new();
    for f in &files {
        let rel = f.new.clone().or_else(|| f.old.clone()).unwrap_or_default();
        let target = resolve_workspace_path(&rel, root);
        let (ok, err) = is_safe_path(&target, root, false);
        if !ok {
            return format!("HATA: {rel}: {err}");
        }
        let mut crlf = false;
        let orig: Vec<String> = if f.old.is_none() {
            Vec::new()
        } else {
            if !target.exists() {
                return format!("HATA: Dosya yok: {rel}");
            }
            let raw =
                String::from_utf8_lossy(&std::fs::read(&target).unwrap_or_default()).into_owned();
            crlf = raw.contains("\r\n");
            raw.replace("\r\n", "\n")
                .split('\n')
                .map(str::to_string)
                .collect()
        };
        let new = match apply_hunks(&orig, &f.hunks) {
            Ok(n) => n,
            Err(e) => return format!("HATA: {rel}: {e}"),
        };
        let text = f
            .new
            .as_ref()
            .map(|_| new.join(if crlf { "\r\n" } else { "\n" }));
        plan.push(Planned {
            target,
            rel,
            text,
            existed: f.old.is_some(),
        });
    }
    let preview = if patch.chars().count() < 6000 {
        patch.to_string()
    } else {
        format!("{}\n...", cut(patch, 6000))
    };
    ctx.emit_info(&format!("Yama önizlemesi:\n{preview}"));
    let req = ApprovalRequest {
        kind: ApprovalKind::Edit,
        ui_kind: "edit",
        title: format!("{} dosyaya yama uygulansın mı?", plan.len()),
        detail: cut(patch, 6000).to_string(),
        default_yes: false,
        proposal: None,
        warning: None,
    };
    if ctx.request_approval(auto_approve, req).is_some() {
        return "İPTAL EDİLDİ: Kullanıcı yamayı onaylamadı.".to_string();
    }
    let mut done: Vec<String> = Vec::new();
    for p in plan {
        if p.existed {
            let _ = backup_file(&p.target, root);
        }
        match p.text {
            None => {
                let _ = std::fs::remove_file(&p.target);
            }
            Some(text) => {
                if let Some(parent) = p.target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&p.target, text.as_bytes()) {
                    return format!("HATA: {}: {e}", p.rel);
                }
            }
        }
        done.push(p.rel);
    }
    format!("{} dosya yamalandı: {}", done.len(), done.join(", "))
}
