//! Çalışma alanı tarayıcı ve dosya ağacı (Python `project_scanner.py`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::tools::file_ops::{safe_read_file, READ_MAX_CHARS};

pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".venv",
    "venv",
    "env",
    "node_modules",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    "dist",
    "build",
    ".evren",
    ".idea",
    ".vscode",
];

pub const CODE_EXTENSIONS: &[&str] = &[
    ".py", ".js", ".ts", ".tsx", ".jsx", ".html", ".css", ".scss", ".json", ".md", ".txt", ".toml",
    ".yaml", ".yml", ".cs", ".cpp", ".c", ".h", ".hpp", ".go", ".rs", ".sql", ".sh", ".ps1", ".bat",
    ".xml", ".ini", ".cfg",
];

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub root: String,
    pub files: Vec<String>,
    pub extension_counts: BTreeMap<String, usize>,
    pub total_files: usize,
    pub total_size_kb: f64,
}

fn ignored(name: &str) -> bool {
    IGNORED_DIRS.contains(&name)
}

fn suffix(path: &Path) -> String {
    path.extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

fn resolved(root: &Path) -> PathBuf {
    crate::tools::file_ops::resolve_path(root)
}

/// Kod/metin dosyalarını tarar (en çok `max_files`, dosya başına `max_file_size_kb`).
pub fn scan_workspace(root: &Path, max_files: usize, max_file_size_kb: f64) -> ScanResult {
    let root = resolved(root);
    let mut files = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0.0;
    let mut stack = vec![root.clone()];
    'walk: while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if ignored(&name) {
                continue;
            }
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            let ext = suffix(&path);
            if !CODE_EXTENSIONS.contains(&ext.as_str())
                && !matches!(name.as_str(), "Dockerfile" | "Makefile" | "LICENSE")
            {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let size_kb = meta.len() as f64 / 1024.0;
            if size_kb > max_file_size_kb {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            files.push(rel);
            let key = if ext.is_empty() { "no_ext".to_string() } else { ext };
            *counts.entry(key).or_insert(0) += 1;
            total += size_kb;
            if files.len() >= max_files {
                break 'walk;
            }
        }
    }
    files.sort();
    ScanResult {
        root: root.to_string_lossy().to_string(),
        total_files: files.len(),
        files,
        extension_counts: counts,
        total_size_kb: (total * 10.0).round() / 10.0,
    }
}

/// ASCII dosya ağacı (klasörler önce, küçük harfe göre sıralı).
pub fn generate_tree(root: &Path, max_depth: usize) -> String {
    let root = resolved(root);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut lines = vec![format!("📁 {name}/")];
    recurse(&root, "", 1, max_depth, &mut lines);
    lines.join("\n")
}

fn recurse(dir: &Path, prefix: &str, depth: usize, max_depth: usize, lines: &mut Vec<String>) {
    if depth > max_depth {
        lines.push(format!("{prefix}└── ... [derinlik sınırı]"));
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(bool, String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if ignored(&name) {
                return None;
            }
            let path = e.path();
            Some((!path.is_dir(), name, path))
        })
        .collect();
    entries.sort_by_key(|a| (a.0, a.1.to_lowercase()));
    let n = entries.len();
    for (i, (is_file, name, path)) in entries.into_iter().enumerate() {
        let last = i + 1 == n;
        let connector = if last { "└── " } else { "├── " };
        let child = if last { "    " } else { "│   " };
        if is_file {
            lines.push(format!("{prefix}{connector}📄 {name}"));
        } else {
            lines.push(format!("{prefix}{connector}📁 {name}/"));
            recurse(&path, &format!("{prefix}{child}"), depth + 1, max_depth, lines);
        }
    }
}

/// Dosyaları okuyup istem bağlamı bloğu kurar.
pub fn build_context_from_files<S: AsRef<str>>(paths: &[S], root: &Path) -> String {
    let root = resolved(root);
    paths
        .iter()
        .map(|fp| {
            let fp = fp.as_ref();
            match safe_read_file(fp, &root, READ_MAX_CHARS) {
                Err(err) => format!("--- DOSYA: {fp} (HATA: {err}) ---"),
                Ok(content) => {
                    let ext = Path::new(fp)
                        .extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_default();
                    format!("--- DOSYA: {fp} ---\n```{ext}\n{content}\n```")
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn scan_skips_ignored_and_binary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/x")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(dir.path().join("Makefile"), "all:").unwrap();
        std::fs::write(dir.path().join("image.png"), [0u8; 4]).unwrap();
        std::fs::write(dir.path().join("node_modules/x/a.js"), "x").unwrap();
        let r = scan_workspace(dir.path(), 150, 250.0);
        assert_eq!(r.files, vec!["Makefile", "src/main.rs"]);
        assert_eq!(r.extension_counts["no_ext"], 1);
        assert_eq!(r.extension_counts[".rs"], 1);
    }

    #[test]
    fn tree_dirs_first_and_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("b/c/d/e")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let tree = generate_tree(dir.path(), 3);
        let lines: Vec<&str> = tree.lines().collect();
        assert!(lines[0].starts_with("📁 "));
        assert_eq!(lines[1], "├── 📁 b/");
        assert!(tree.contains("[derinlik sınırı]"));
        assert_eq!(*lines.last().unwrap(), "└── 📄 a.txt");
    }

    #[test]
    fn context_block_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "print(1)").unwrap();
        let ctx = build_context_from_files(&["a.py", "yok.py"], dir.path());
        assert!(ctx.starts_with("--- DOSYA: a.py ---\n```py\nprint(1)\n```"));
        assert!(ctx.contains("--- DOSYA: yok.py (HATA: "));
    }
}
