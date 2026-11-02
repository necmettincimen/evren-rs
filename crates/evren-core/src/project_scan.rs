//! Çalışma alanı tarayıcı ve dosya ağacı (Python `project_scanner.py`).

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
    ".yaml", ".yml", ".cs", ".cpp", ".c", ".h", ".hpp", ".go", ".rs", ".sql", ".sh", ".ps1",
    ".bat", ".xml", ".ini", ".cfg",
];

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub root: String,
    pub files: Vec<String>,
    /// Uzantı → dosya sayısı, ilk görülme sırasıyla (Python `dict`).
    pub extension_counts: Vec<(String, usize)>,
    pub total_files: usize,
    pub total_size_kb: f64,
}

impl ScanResult {
    /// Uzantının dosya sayısı (yoksa 0).
    pub fn ext_count(&self, ext: &str) -> usize {
        self.extension_counts
            .iter()
            .find(|(e, _)| e == ext)
            .map_or(0, |(_, c)| *c)
    }
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

/// Dizinin (yok sayılmayan) dosyaları ve alt dizinleri; NTFS'teki gibi büyük/küçük
/// harf duyarsız ad sırası.
fn list_dir(dir: &Path) -> (Vec<std::fs::DirEntry>, Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut entries: Vec<_> = rd
        .flatten()
        .filter(|e| !ignored(&e.file_name().to_string_lossy()))
        .collect();
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_uppercase());
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in entries {
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => dirs.push(entry.path()),
            Ok(_) => files.push(entry),
            Err(_) => {}
        }
    }
    (files, dirs)
}

/// Kod/metin dosyalarını tarar (en çok `max_files`, dosya başına `max_file_size_kb`).
pub fn scan_workspace(root: &Path, max_files: usize, max_file_size_kb: f64) -> ScanResult {
    let root = resolved(root);
    let mut files = Vec::new();
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut total = 0.0;
    let mut add = |entry: &std::fs::DirEntry| -> bool {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let ext = suffix(&path);
        if !CODE_EXTENSIONS.contains(&ext.as_str())
            && !matches!(name.as_str(), "Dockerfile" | "Makefile" | "LICENSE")
        {
            return false;
        }
        let Ok(meta) = entry.metadata() else {
            return false;
        };
        let size_kb = meta.len() as f64 / 1024.0;
        if size_kb > max_file_size_kb {
            return false;
        }
        let rel = path
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        files.push(rel);
        let key = if ext.is_empty() {
            "no_ext".to_string()
        } else {
            ext
        };
        match counts.iter_mut().find(|(e, _)| *e == key) {
            Some((_, c)) => *c += 1,
            None => counts.push((key, 1)),
        }
        total += size_kb;
        files.len() >= max_files
    };
    // Python 3.12 `rglob("*")` sırası: kökün dosyaları, sonra `Path.walk` (derinlik
    // öncelikli) ile gezilen her dizinin tüm alt dizinlerinin dosyaları sırayla.
    let (root_files, root_dirs) = list_dir(&root);
    let full = root_files.iter().any(&mut add);
    let mut walk = if full { Vec::new() } else { vec![root_dirs] };
    'walk: while let Some(children) = walk.pop() {
        let mut grandchildren = Vec::with_capacity(children.len());
        for child in children {
            let (child_files, child_dirs) = list_dir(&child);
            if child_files.iter().any(&mut add) {
                break 'walk;
            }
            grandchildren.push(child_dirs);
        }
        walk.extend(grandchildren.into_iter().rev());
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
            recurse(
                &path,
                &format!("{prefix}{child}"),
                depth + 1,
                max_depth,
                lines,
            );
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
        assert_eq!(r.ext_count("no_ext"), 1);
        assert_eq!(r.ext_count(".rs"), 1);
    }

    #[test]
    fn extension_counts_keep_first_seen_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a_dir")).unwrap();
        std::fs::write(dir.path().join("z.py"), "x").unwrap();
        std::fs::write(dir.path().join("b.toml"), "x").unwrap();
        std::fs::write(dir.path().join("a_dir/m.rs"), "x").unwrap();
        std::fs::write(dir.path().join("a_dir/n.py"), "x").unwrap();
        let r = scan_workspace(dir.path(), 150, 250.0);
        let order: Vec<&str> = r.extension_counts.iter().map(|(e, _)| e.as_str()).collect();
        // Kök dosyaları (b.toml, z.py) alt dizinden (a_dir) önce gelir.
        assert_eq!(order, vec![".toml", ".py", ".rs"]);
        assert_eq!(r.ext_count(".py"), 2);
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
