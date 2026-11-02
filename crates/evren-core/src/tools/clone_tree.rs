//! `clone_tree` aracı (Python `clone_tree.py`): dizini kopyalayıp ad ve
//! içerikte toplu yeniden adlandırma.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;

use super::file_ops::{is_safe_path, resolve_path};
use super::ToolContext;

pub const DEFAULT_EXCLUDE: &[&str] = &[
    ".git",
    "bin",
    "obj",
    "node_modules",
    "__pycache__",
    ".vs",
    ".idea",
];

/// Tek geçişte sıralı değiştirme (`_apply`): önceki değişimin çıktısı yeniden dönüştürülmez.
pub fn apply_replacements(text: &str, reps: &[(String, String)]) -> String {
    let keys: Vec<&str> = reps
        .iter()
        .filter(|(a, _)| !a.is_empty())
        .map(|(a, _)| a.as_str())
        .collect();
    if keys.is_empty() {
        return text.to_string();
    }
    let pattern = keys
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|");
    let Ok(re) = Regex::new(&pattern) else {
        return text.to_string();
    };
    re.replace_all(text, |caps: &regex::Captures| {
        let m = &caps[0];
        reps.iter()
            .rev()
            .find(|(a, _)| a == m)
            .map(|(_, b)| b.clone())
            .unwrap_or_else(|| m.to_string())
    })
    .into_owned()
}

fn is_text(data: &[u8]) -> bool {
    !data[..data.len().min(4096)].contains(&0)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        out.push(p.clone());
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            walk(&p, out);
        }
    }
}

/// `tool_clone_tree(arguments, root)`.
pub fn tool_clone_tree(ctx: &ToolContext, arguments: &Value) -> String {
    let root = resolve_path(&ctx.root());
    let get_str = |k: &str| {
        arguments.get(k).map(|v| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        })
    };
    let src = resolve_path(&root.join(get_str("src").unwrap_or_default()));
    let dst = resolve_path(&root.join(get_str("dst").unwrap_or_default()));
    for (p, ro) in [(&src, true), (&dst, false)] {
        let (ok, reason) = is_safe_path(p, &root, ro);
        if !ok {
            return format!("HATA: yol reddedildi: {} ({reason})", p.display());
        }
    }
    if !src.is_dir() {
        return format!("HATA: kaynak dizin yok: {}", src.display());
    }
    if dst == src || dst.starts_with(&src) {
        return "HATA: hedef kaynağın içinde/aynısı olamaz.".to_string();
    }
    let reps: Vec<(String, String)> = arguments
        .get("replacements")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|pair| {
                    let items = pair.as_array()?;
                    let a = items.first()?;
                    let b = items.get(1)?;
                    let to_s = |v: &Value| {
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string())
                    };
                    let a = to_s(a);
                    if a.is_empty() {
                        return None;
                    }
                    Some((a, to_s(b)))
                })
                .collect()
        })
        .unwrap_or_default();
    if reps.is_empty() {
        return "HATA: replacements boş.".to_string();
    }
    let excl: HashSet<String> = match arguments.get("exclude").and_then(Value::as_array) {
        Some(arr) if !arr.is_empty() => arr
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .collect(),
        _ => DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect(),
    };
    let dry = arguments
        .get("dry_run")
        .map(super::args::is_truthy)
        .unwrap_or(false);
    let (mut files, mut changed, mut skipped) = (0usize, 0usize, 0usize);
    let mut all = Vec::new();
    walk(&src, &mut all);
    all.sort();
    for path in all {
        let rel = path.strip_prefix(&src).unwrap_or(&path);
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.iter().any(|p| excl.contains(p)) || path.is_dir() {
            continue;
        }
        let mut target = dst.clone();
        for part in &parts {
            target.push(apply_replacements(part, &reps));
        }
        files += 1;
        if target.exists() {
            skipped += 1;
            continue;
        }
        let mut data = std::fs::read(&path).unwrap_or_default();
        if is_text(&data) {
            if let Ok(text) = std::str::from_utf8(&data) {
                let new = apply_replacements(text, &reps);
                if new != text {
                    changed += 1;
                }
                data = new.into_bytes();
            }
        }
        if !dry {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&target, &data);
        }
    }
    let verb = if dry {
        "KURU ÇALIŞTIRMA"
    } else {
        "BAŞARILI"
    };
    format!(
        "{verb}: {files} dosya ({changed} içerik dönüştü, {skipped} atlandı/mevcut) -> {}",
        dst.display()
    )
}
