//! Proje düzeyi akıllı bağlam belgeleri (`agent.md`, `changelog.md`; Python
//! `project_docs.py`). Belgeler CLI'nin çalıştığı dizinde durur; ajan nerede
//! olduğunu, projenin neye benzediğini, hangi kuralların geçerli olduğunu ve
//! burada en son ne yapıldığını bilir.
//!
//! Güvenlik sözleşmesi:
//! - Eksik dosya şablondan oluşturulur.
//! - Var olan dosyada yalnız `evren:managed` bölümleri, içerik gerçekten
//!   değiştiyse yeniden yazılır; insan metnine dokunulmaz.
//! - İşaretsiz dosyaya `adopt` istenmedikçe dokunulmaz.
//! - NTFS büyük/küçük harf duyarsızdır: proje `AGENTS.md` taşıyorsa `agent.md`
//!   yazmak onu ezer; bu durumda `evren-agent.md` kullanılır.
//! - Her yazım `safe_write_file` üzerinden yapılır, her gövde `redact_text`ten geçer.
//! - G/Ç hataları özelliği kapatır; CLI belgeler yüzünden çökmez.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::Utc;
use serde_json::{json, Map, Value};

use crate::config::{get_evren_dir, Config};
use crate::docs_merge::{
    extract_sections, has_managed_sections, render_sections, section_hash, strip_managed,
};
use crate::docs_state::{load_state, save_state, state_path};
use crate::redact::redact_text;
use crate::tools::file_ops::{backup_enabled, resolve_path, safe_write_file};

pub const AGENT_DOC_NAME: &str = "agent.md";
pub const AGENT_DOC_ALT_NAME: &str = "evren-agent.md";
pub const CHANGELOG_NAME: &str = "changelog.md";
pub const CHANGELOG_ALT_NAME: &str = "evren-changelog.md";
const LOCK_FILE: &str = "docs.lock";
const WARN_LOG: &str = "logs/docs_warn.log";
const AGENTS_REFERENCE_NAME: &str = "AGENTS.md";

const LOCK_TTL_SECONDS: u64 = 120;
const MAX_MAP_LINES: usize = 25;
/// CLI'nin kendi belgeleri proje haritasında görünmez; yoksa harita yazıldığı
/// anda değişir ve her çalıştırma `git status`'u kirletir.
const OWN_DOC_NAMES: [&str; 4] = [
    "agent.md",
    "evren-agent.md",
    "changelog.md",
    "evren-changelog.md",
];
const MAX_CHANGELOG_ENTRY_CHARS: usize = 1200;
const RECENT_SESSION_ROWS: usize = 10;
const MAX_AGENTS_REFERENCE_CHARS: usize = 1500;

/// İşaretsiz belgeyi benimserken bölüm sırası.
pub const AGENT_SECTIONS: [&str; 5] = [
    "identity",
    "runtime_env",
    "project_map",
    "conventions",
    "current_focus",
];

/// Prompt'a enjeksiyon sırası: küçük ve yönlendirici bölümler önce; en hacimli
/// ve en bayat `project_map` sonda durur, bütçe yetmezse ilk o düşer.
const CONTEXT_SECTION_PRIORITY: [&str; 5] = [
    "conventions",
    "current_focus",
    "identity",
    "runtime_env",
    "project_map",
];
const CHANGELOG_SECTIONS: [&str; 1] = ["recent"];

/// Varsayılan bağlam bütçesi (token); her adımda yeniden gönderilir.
pub const DEFAULT_DOCS_TOKENS: i64 = 1200;

/// Bir `ensure_project_docs` çalıştırmasının sonucu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocsResult {
    pub root: String,
    pub agent_doc: String,
    pub changelog: String,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub notes: Vec<String>,
    pub disabled: bool,
    pub reason: String,
    pub conflict: String,
}

impl DocsResult {
    fn new(root: &Path) -> Self {
        DocsResult {
            root: root.to_string_lossy().to_string(),
            ..Default::default()
        }
    }

    pub fn changed(&self) -> bool {
        !self.created.is_empty() || !self.updated.is_empty()
    }

    /// Açılış satırı için kısa özet.
    pub fn summary(&self) -> String {
        if self.disabled {
            return if self.reason.is_empty() {
                "kapalı".to_string()
            } else {
                format!("kapalı ({})", self.reason)
            };
        }
        let mut names: Vec<&str> = Vec::new();
        for name in self.created.iter().chain(self.updated.iter()) {
            let base = name.split('#').next().unwrap_or(name);
            if !names.contains(&base) {
                names.push(base);
            }
        }
        if names.is_empty() {
            "güncel".to_string()
        } else {
            format!("hazır: {}", names.join(", "))
        }
    }

    /// Açılışta gösterilecek notlar; ad çakışması yalnız oluşturmada.
    pub fn details(&self) -> Vec<String> {
        let mut out = self.notes.clone();
        if !self.conflict.is_empty() && !self.created.is_empty() {
            out.insert(0, self.conflict.clone());
        }
        out
    }
}

/// `ensure_project_docs` seçenekleri.
#[derive(Debug, Clone, Default)]
pub struct DocsOptions {
    pub goal: String,
    pub status: String,
    pub refresh: bool,
    pub adopt: bool,
    pub read_only: bool,
}

// ------------------------------------------------------------ etkinlik

/// (etkin, neden). Kapatma anahtarı: `EVREN_AUTO_DOCS=false`.
pub fn docs_enabled(root: &Path, config: &Config) -> (bool, String) {
    if !config.flag("EVREN_AUTO_DOCS", true) {
        return (false, "EVREN_AUTO_DOCS=false".to_string());
    }
    let path = get_evren_dir(root).join("docs.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (true, String::new());
    };
    let raw = raw.trim_start_matches('\u{feff}');
    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(raw) {
        if m.get("disabled") == Some(&Value::Bool(true)) {
            return (false, ".evren/docs.json disabled=true".to_string());
        }
    }
    (true, String::new())
}

/// Enjekte edilen tüm belge blokları için token tavanı (0 kapatır).
pub fn docs_token_budget(config: &Config) -> i64 {
    let raw = config.get("EVREN_DOCS_TOKENS").unwrap_or("").trim();
    if raw.is_empty() {
        return DEFAULT_DOCS_TOKENS;
    }
    raw.parse::<i64>()
        .map(|v| v.max(0))
        .unwrap_or(DEFAULT_DOCS_TOKENS)
}

// ------------------------------------------------------------ yardımcılar

fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

fn stamp() -> String {
    format!(
        "{} (EVREN CLI {})",
        Utc::now().format("%Y-%m-%d %H:%M UTC"),
        crate::VERSION
    )
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn take(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

fn take_last(text: &str, n: usize) -> String {
    let total = chars(text);
    text.chars().skip(total.saturating_sub(n)).collect()
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, limit: usize) -> String {
    let text = squash(text);
    if chars(&text) <= limit {
        return text;
    }
    format!("{}…", take(&text, limit.saturating_sub(1)).trim_end())
}

fn warn(root: &Path, message: &str) {
    use std::io::Write;
    let path = get_evren_dir(root).join(WARN_LOG);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{} {message}", now_iso());
    }
}

/// Eşzamanlı oturumlar için en iyi çaba tek yazar ipucu.
fn acquire_lock(root: &Path) -> bool {
    let path = get_evren_dir(root).join(LOCK_FILE);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return true;
        }
    }
    if let Ok(meta) = std::fs::metadata(&path) {
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .unwrap_or(Duration::ZERO);
        if age < Duration::from_secs(LOCK_TTL_SECONDS) {
            return false;
        }
    }
    let _ = std::fs::write(&path, format!("{} {}\n", std::process::id(), now_iso()));
    true
}

fn release_lock(root: &Path) {
    let _ = std::fs::remove_file(get_evren_dir(root).join(LOCK_FILE));
}

fn file_names(root: &Path) -> Option<BTreeSet<String>> {
    let rd = std::fs::read_dir(root).ok()?;
    Some(
        rd.flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
    )
}

fn lowered(names: &BTreeSet<String>) -> BTreeMap<String, String> {
    names
        .iter()
        .map(|n| (n.to_lowercase(), n.clone()))
        .collect()
}

/// Farklı harfli kardeşi ezemeyecek ajan belgesi adı: (ad, çakışma notu).
pub fn resolve_doc_name(root: &Path, wanted: &str) -> (String, String) {
    let Some(names) = file_names(root) else {
        return (wanted.to_string(), String::new());
    };
    if names.contains(wanted) {
        return (wanted.to_string(), String::new());
    }
    let low = lowered(&names);
    if let Some(clash) = low.get(&wanted.to_lowercase()) {
        if clash != wanted {
            return (
                AGENT_DOC_ALT_NAME.to_string(),
                format!(
                    "'{clash}' zaten var; NTFS büyük/küçük harf duyarsız olduğu için \
                     '{wanted}' yerine '{AGENT_DOC_ALT_NAME}' kullanılıyor"
                ),
            );
        }
    }
    // Önceki bir çalıştırma alternatif adı seçmiş: onu kullanmaya devam et.
    if low.contains_key(AGENT_DOC_ALT_NAME) {
        return (AGENT_DOC_ALT_NAME.to_string(), String::new());
    }
    (wanted.to_string(), String::new())
}

pub fn agent_doc_path(root: &Path) -> PathBuf {
    root.join(resolve_doc_name(root, AGENT_DOC_NAME).0)
}

/// Farklı harfli kardeşe (`CHANGELOG.md`) yazmayan changelog hedefi.
pub fn changelog_path(root: &Path) -> PathBuf {
    let Some(names) = file_names(root) else {
        return root.join(CHANGELOG_NAME);
    };
    if names.contains(CHANGELOG_NAME) {
        return root.join(CHANGELOG_NAME);
    }
    let low = lowered(&names);
    if low.get(CHANGELOG_NAME).is_some_and(|c| c != CHANGELOG_NAME) {
        return root.join(CHANGELOG_ALT_NAME);
    }
    if low.contains_key(CHANGELOG_ALT_NAME) {
        return root.join(CHANGELOG_ALT_NAME);
    }
    root.join(CHANGELOG_NAME)
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim_start_matches('\u{feff}').to_string())
        .unwrap_or_default()
}

fn write_text(root: &Path, path: &Path, text: &str) -> bool {
    let backup = path.exists() && backup_enabled();
    match safe_write_file(&path.to_string_lossy(), text, root, Some(backup), None) {
        Ok(_) => true,
        Err((_, error)) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            warn(root, &format!("yazma reddedildi {name}: {error}"));
            false
        }
    }
}

// ------------------------------------------------------------ bölüm gövdeleri

fn git_info(root: &Path) -> (String, String) {
    let t = Duration::from_secs(5);
    let Some(branch) = crate::git::git_run(&["rev-parse", "--abbrev-ref", "HEAD"], root, t) else {
        return ("git yok".to_string(), String::new());
    };
    let commit = crate::git::git_run(&["log", "-1", "--format=%h %ad", "--date=short"], root, t)
        .unwrap_or_default();
    let branch = if branch.is_empty() {
        "git yok".to_string()
    } else {
        branch
    };
    (branch, commit)
}

pub fn identity_body(doc_name: &str) -> String {
    [
        format!("Bu dosya EVREN CLI tarafından otomatik yönetilir (`{doc_name}`)."),
        "`evren:managed` işaretli bloklar CLI her çalıştığında yenilenir;".to_string(),
        "bu blokların dışındaki metin korunur ve ajana bağlam olarak verilir.".to_string(),
        "Canlı ortam her zaman bu dosyadan önceliklidir: yol/araç burada yazsa bile".to_string(),
        "uygulamadan önce salt-okuma ile doğrula.".to_string(),
    ]
    .join("\n")
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "linux" => "Linux",
        "macos" => "Darwin",
        other => other,
    }
}

pub fn runtime_env_body(root: &Path, config: &Config) -> String {
    let shell = if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        "/bin/sh".to_string()
    };
    let shell_name = Path::new(&shell)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or(shell.clone());
    let (branch, commit) = git_info(root);
    let mut git = format!("- Git dalı: {branch}");
    if !commit.is_empty() {
        git.push_str(&format!(" | son commit: {commit}"));
    }
    [
        format!("- Çalışma alanı: {}", root.display()),
        format!(
            "- İşletim sistemi / kabuk: {} / {shell_name} (PowerShell gerekiyorsa \
             `powershell -NoProfile -Command` ile açıkça çağır)",
            os_name()
        ),
        format!("- Çalışma zamanı: Rust | CLI sürümü: {}", crate::VERSION),
        git,
        format!("- Varsayılan model: {}", config.default_model()),
    ]
    .join("\n")
}

/// CLI'nin bu proje dizininde kendi ürettiği girdiler (küçük harf).
fn cli_owned_entries(root: &Path) -> BTreeSet<String> {
    let mut owned: BTreeSet<String> = ["skills.md", "commands.md", "lessons.md"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let marker = read_text(&root.join("context").join("README.md"));
    if marker.starts_with("# EVREN CLI destek dosyalar") {
        owned.insert("context".into());
        owned.insert("logs".into());
    }
    owned
}

fn is_cli_owned_path(rel_path: &str, cli_owned: &BTreeSet<String>) -> bool {
    rel_path
        .split(['/', '\\'])
        .any(|part| cli_owned.contains(&part.to_lowercase()))
}

fn own_docs_size_kb(root: &Path) -> f64 {
    OWN_DOC_NAMES
        .iter()
        .filter_map(|n| std::fs::metadata(root.join(n)).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len() as f64 / 1024.0)
        .sum()
}

/// `generate_tree` satırından küçük harfli girdi adı ('' yoksa).
fn tree_line_name(line: &str) -> String {
    for connector in ["└── ", "├── "] {
        if let Some((_, rest)) = line.split_once(connector) {
            let name = rest.trim().trim_start_matches(['📄', '📁', ' ']);
            return name.trim_end_matches('/').to_lowercase();
        }
    }
    String::new()
}

/// Python `round(x, 1)` yazımı.
fn py_float1(v: f64) -> String {
    format!("{:.1}", (v * 10.0).round() / 10.0)
}

pub fn project_map_body(root: &Path) -> String {
    let stats = crate::project_scan::scan_workspace(root, 400, 250.0);
    let tree = crate::project_scan::generate_tree(root, 2);
    let cli_owned = cli_owned_entries(root);
    let base_name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
    let files: Vec<String> = stats
        .files
        .iter()
        .filter(|f| {
            !OWN_DOC_NAMES.contains(&base_name(f).as_str()) && !is_cli_owned_path(f, &cli_owned)
        })
        .cloned()
        .collect();
    let own_count = stats.files.len() - files.len();
    let mut exts = stats.extension_counts.clone();
    if own_count > 0 {
        let md = exts
            .get(".md")
            .copied()
            .unwrap_or(0)
            .saturating_sub(own_count);
        if md == 0 {
            exts.remove(".md");
        } else {
            exts.insert(".md".into(), md);
        }
    }
    let size_kb = (stats.total_size_kb - own_docs_size_kb(root)).max(0.0);
    let mut top: Vec<(&String, &usize)> = exts.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let top: Vec<String> = top
        .iter()
        .take(8)
        .map(|(e, c)| format!("{e} {c}"))
        .collect();
    let mut lines = vec![
        format!(
            "- Dosya sayısı (taranan): {} | boyut: {} KB",
            files.len(),
            py_float1(size_kb)
        ),
        format!(
            "- Uzantılar: {}",
            if top.is_empty() {
                "yok".to_string()
            } else {
                top.join(", ")
            }
        ),
    ];
    let entry = entry_points(root, &files);
    if !entry.is_empty() {
        lines.push(format!("- Giriş noktası: {entry}"));
    }
    lines.push(format!("- Komutlar: {}", command_hints(root)));
    lines.push("- Ağaç (derinlik 2):".to_string());
    let tree_lines: Vec<&str> = tree
        .split('\n')
        .filter(|l| {
            let n = tree_line_name(l);
            !OWN_DOC_NAMES.contains(&n.as_str()) && !cli_owned.contains(&n)
        })
        .take(MAX_MAP_LINES)
        .collect();
    let full = tree_lines.len() >= MAX_MAP_LINES;
    lines.extend(tree_lines.into_iter().map(|l| format!("  {l}")));
    if full {
        lines.push("  … [ağaç kırpıldı; tam liste için `evren scan`]".to_string());
    }
    lines.join("\n")
}

fn entry_points(root: &Path, files: &[String]) -> String {
    let mut hints: Vec<String> = Vec::new();
    for manifest in ["pyproject.toml", "Cargo.toml"] {
        if root.join(manifest).exists() {
            hints.push(format!("`{manifest}`"));
        }
    }
    for candidate in [
        "src/cli.py",
        "cli.py",
        "main.py",
        "manage.py",
        "package.json",
        "src/main.rs",
    ] {
        if files.iter().any(|f| f == candidate) || root.join(candidate).exists() {
            hints.push(format!("`{candidate}`"));
        }
    }
    let packages: BTreeSet<&str> = files
        .iter()
        .filter_map(|p| p.split_once('/').map(|(a, _)| a))
        .collect();
    let packages: Vec<String> = packages.iter().take(6).map(|n| format!("`{n}/`")).collect();
    if !packages.is_empty() {
        hints.push(format!("paketler: {}", packages.join(", ")));
    }
    hints.join(" | ")
}

fn command_hints(root: &Path) -> &'static str {
    if root.join("Cargo.toml").exists() {
        return "`cargo test` (test), `cargo build` (derleme)";
    }
    if root.join("pyproject.toml").exists()
        || root.join("pytest.ini").exists()
        || root.join("tests").is_dir()
    {
        return "`python -m pytest -q` (test), `python -m compileall src` (sözdizimi)";
    }
    if root.join("package.json").exists() {
        return "`npm test` / `npm run build` (package.json betiklerine bakın)";
    }
    "proje betiği algılanmadı; README'ye bakın"
}

pub fn conventions_body() -> String {
    [
        "- Değiştirmeden önce oku (`view_file`), hedefli `edit_file` kullan, sonra test et.",
        "- Yeni kod mevcut stille uyumlu olmalı: docstring, tip imzaları, girinti.",
        "- `.env` ve `PROTECTED_FILES` düzenlenmez; sır/anahtar hiçbir belgeye yazılmaz.",
        "- Windows: yol ayıracı `\\`, kabuk `cmd.exe`; `safe_write_file` BOM/CRLF'yi korur.",
        "- Bu dosyanın yönetilen bloklarını `edit_file` ile elle değiştirmeye çalışma;",
        "  gerekirse `/docs refresh` veya `/docs unmark` kullan.",
    ]
    .join("\n")
}

fn session_of(state: &Map<String, Value>) -> Map<String, Value> {
    match state.get("session") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or("")
}

fn or_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() {
        default
    } else {
        value
    }
}

pub fn current_focus_body(root: &Path, goal: &str, status: &str) -> String {
    let session = session_of(&load_state(root));
    let goal = goal.trim();
    let last_goal = or_default(
        or_default(goal, str_of(&session, "last_goal")),
        "(henüz görev kaydı yok)",
    );
    let status = status.trim();
    let mut lines = vec![
        format!(
            "- Son oturum: {}",
            or_default(str_of(&session, "last_run"), "(kayıt yok)")
        ),
        format!("- Son görev: {}", clip(last_goal, 120)),
        format!(
            "- Durum: {}",
            or_default(
                or_default(status, str_of(&session, "last_status")),
                "bilinmiyor"
            )
        ),
    ];
    let blockers = open_blockers(root);
    if !blockers.is_empty() {
        lines.push(format!("- Açık engeller: {}", blockers.join("; ")));
    }
    lines.join("\n")
}

fn open_blockers(root: &Path) -> Vec<String> {
    let tasks = crate::tracking::list_tasks("all", root);
    let Ok(flagged) = crate::workflow_state::review_tasks(&tasks, None) else {
        return Vec::new();
    };
    flagged
        .iter()
        .filter(|t| {
            crate::workflow_state::flags_of(t)
                .iter()
                .any(|f| f == "blocked")
        })
        .take(5)
        .map(|t| {
            format!(
                "{}: {}",
                crate::tracking::str_field(t, "id"),
                clip(crate::tracking::str_field(t, "description"), 80)
            )
        })
        .collect()
}

// ------------------------------------------------------------ agent.md

fn sections_map(pairs: [(&str, String); 5]) -> BTreeMap<String, String> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn agent_skeleton(root: &Path, doc_name: &str, config: &Config) -> String {
    let sections = sections_map([
        ("identity", identity_body(doc_name)),
        ("runtime_env", runtime_env_body(root, config)),
        ("project_map", project_map_body(root)),
        ("conventions", conventions_body()),
        ("current_focus", current_focus_body(root, "", "")),
    ]);
    let body = render_sections("", &sections, true, &AGENT_SECTIONS).text;
    let title = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "proje".to_string());
    let lines = [
        format!("# {doc_name} — {title}"),
        String::new(),
        body,
        String::new(),
        "## Amaç".to_string(),
        String::new(),
        "(Bu bölümü siz yazarsınız; CLI dokunmaz. Bu projede ajanın hedefini ve".to_string(),
        "sizin önceliklerinizi buraya not edin.)".to_string(),
        String::new(),
        "## Notlar (elle)".to_string(),
        String::new(),
        "(Buraya yazılanlar korunur ve ajana bağlam olarak gösterilir.)".to_string(),
        String::new(),
    ];
    lines.join("\n") + "\n"
}

/// `agent.md` için tüm yönetilen gövdeler (maskelenmiş).
pub fn agent_sections(
    root: &Path,
    goal: &str,
    status: &str,
    doc_name: &str,
    config: &Config,
) -> BTreeMap<String, String> {
    sections_map([
        ("identity", redact_text(&identity_body(doc_name))),
        ("runtime_env", redact_text(&runtime_env_body(root, config))),
        ("project_map", redact_text(&project_map_body(root))),
        ("conventions", redact_text(&conventions_body())),
        (
            "current_focus",
            redact_text(&current_focus_body(root, goal, status)),
        ),
    ])
}

fn ensure_agent_doc(root: &Path, config: &Config, opts: &DocsOptions, result: &mut DocsResult) {
    let path = agent_doc_path(root);
    let doc_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| AGENT_DOC_NAME.to_string());
    result.agent_doc = doc_name.clone();
    let (_name, conflict) = resolve_doc_name(root, AGENT_DOC_NAME);
    if !conflict.is_empty() && doc_name != AGENT_DOC_NAME {
        result.conflict = format!("{conflict}; '{AGENTS_REFERENCE_NAME}' dosyasına dokunulmadı");
    }

    if !path.exists() {
        let text = redact_text(&agent_skeleton(root, &doc_name, config));
        if write_text(root, &path, &text) {
            result.created.push(doc_name.clone());
            remember_hashes(root, &doc_name, &text);
        }
        return;
    }

    let current = read_text(&path);
    let sections = agent_sections(root, &opts.goal, &opts.status, &doc_name, config);
    let rendered = if !has_managed_sections(&current) {
        if !(opts.adopt || opts.refresh) {
            result.skipped.push(doc_name.clone());
            result.notes.push(format!(
                "{doc_name} içinde yönetilen blok yok; eklemek için `/docs refresh --adopt`"
            ));
            warn(
                root,
                &format!("{doc_name}: yönetilen blok yok, dokunulmadı"),
            );
            return;
        }
        render_sections(&current, &sections, true, &AGENT_SECTIONS)
    } else {
        render_sections(&current, &sections, false, &[])
    };
    if let Some(err) = rendered.errors.first() {
        result.skipped.push(doc_name.clone());
        result
            .notes
            .push(format!("{doc_name}: {err} (dokunulmadı)"));
        warn(root, &format!("{doc_name}: {err}"));
        return;
    }
    if rendered.changed.is_empty() {
        result.skipped.push(doc_name);
        return;
    }
    if write_text(root, &path, &rendered.text) {
        result
            .updated
            .extend(rendered.changed.iter().map(|n| format!("{doc_name}#{n}")));
        remember_hashes(root, &doc_name, &rendered.text);
    }
}

fn remember_hashes(root: &Path, doc_name: &str, text: &str) {
    let mut state = load_state(root);
    let mut sections = match state.remove("sections") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for (name, body) in extract_sections(text) {
        sections.insert(format!("{doc_name}.{name}"), json!(section_hash(&body)));
    }
    state.insert("sections".into(), Value::Object(sections));
    state.insert("updated_at".into(), json!(now_iso()));
    let _ = save_state(root, &state);
}

// ------------------------------------------------------------ changelog.md

fn root_title(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "proje".to_string())
}

fn changelog_skeleton(root: &Path) -> String {
    let recent = BTreeMap::from([("recent".to_string(), "- (henüz kayıt yok)".to_string())]);
    let body = render_sections("", &recent, true, &CHANGELOG_SECTIONS).text;
    let lines = [
        format!("# Changelog — {}", root_title(root)),
        String::new(),
        "Bu dosya EVREN CLI tarafından **append-only** güncellenir; eski girişler".to_string(),
        "değiştirilmez ve silinmez. Aşağıdaki yönetilen blok son oturumların".to_string(),
        "özetidir ve CLI tarafından yeniden yazılır.".to_string(),
        String::new(),
        body,
        String::new(),
    ];
    lines.join("\n") + "\n"
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn entry_signature(entry: &Map<String, Value>) -> String {
    let mut files = str_list(entry.get("files"));
    files.sort();
    let parts = [
        take(&squash(str_of(entry, "goal")), 400),
        files.join("|"),
        str_list(entry.get("exit_codes")).join(","),
    ];
    section_hash(&parts.join("::"))
}

/// Prompt çerçevesi şablonunu atıp gerçek görevi bırakır.
pub fn clean_goal(text: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            r"(?i)^(asıl görev|görev)\s*(\(geçmiş bağlam\))?\s*:?\s*",
            r"(?i)^.*?çıktı, format ve teslim yüzeyidir\.\s*(brief:)?\s*",
            r"(?i)^brief\s*:\s*",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    });
    let mut goal = squash(text);
    for _ in 0..3 {
        let before = goal.clone();
        for re in res {
            goal = re.replace(&goal, "").into_owned();
        }
        if goal == before {
            break;
        }
    }
    goal.trim_matches([' ', '.', '-']).to_string()
}

/// Bir oturum girdisi ekler. Hiçbir şey yazılmadıysa `false`.
/// `entry`: `goal`, `files`, `exit_codes`, `verification` alanları (Python dict).
pub fn append_changelog_entry(root: &Path, config: &Config, entry: &Value) -> bool {
    let Value::Object(entry) = entry else {
        return false;
    };
    // EVREN_AUTO_DOCS=false yalnız önyüklemeyi değil her yazımı durdurur.
    if !docs_enabled(root, config).0 {
        return false;
    }
    let signature = entry_signature(entry);
    let state = load_state(root);
    if state.get("last_changelog_entry").and_then(Value::as_str) == Some(signature.as_str()) {
        return false;
    }
    let path = changelog_path(root);
    let created = !path.exists();
    let text = if created {
        changelog_skeleton(root)
    } else {
        read_text(&path)
    };

    let goal = clip(&clean_goal(&redact_text(str_of(entry, "goal"))), 160);
    let goal = or_default(&goal, "(görev belirtilmedi)").to_string();
    let files = str_list(entry.get("files"));
    let verification = match entry.get("verification") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    let verification = clip(&redact_text(&verification), 200);

    // Her giriş üç alan taşır; `commands`/`exit_codes` yalnız imzaya girer.
    let files_text = if files.is_empty() {
        "dosya değişikliği yok".to_string()
    } else {
        files
            .iter()
            .take(12)
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let block = [
        format!("## {}", stamp()),
        String::new(),
        format!("- Görev: {goal}"),
        format!("- Değişen dosyalar: {files_text}"),
        format!(
            "- Doğrulama: {}",
            or_default(&verification, "doğrulama çalıştırılmadı")
        ),
    ];
    let mut chunk = block.join("\n");
    if chars(&chunk) > MAX_CHANGELOG_ENTRY_CHARS {
        chunk = format!("{} …", take(&chunk, MAX_CHANGELOG_ENTRY_CHARS).trim_end());
    }

    let recent = recent_lines(&text, &goal, &files);
    let rendered = render_sections(
        &text,
        &BTreeMap::from([("recent".to_string(), recent)]),
        false,
        &[],
    );
    if let Some(err) = rendered.errors.first() {
        warn(root, &format!("{CHANGELOG_NAME}: {err}"));
        return false;
    }
    let new_text = format!("{}\n\n{chunk}\n", rendered.text.trim_end_matches('\n'));
    if !write_text(root, &path, &redact_text(&new_text)) {
        return false;
    }
    let mut state = load_state(root);
    state.insert("last_changelog_entry".into(), json!(signature));
    state.insert("updated_at".into(), json!(now_iso()));
    let _ = save_state(root, &state);
    true
}

/// Yönetilen `recent` bloğunu yeniden kurar (en yeni sonda, sınırlı satır).
fn recent_lines(text: &str, goal: &str, files: &[String]) -> String {
    let existing = extract_sections(text).remove("recent").unwrap_or_default();
    let mut rows: Vec<String> = existing
        .split('\n')
        .filter(|l| l.trim().starts_with('-') && !l.contains("henüz kayıt yok"))
        .map(str::to_string)
        .collect();
    let mut files_part = files
        .iter()
        .take(4)
        .map(|f| format!("`{f}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if files_part.is_empty() {
        files_part = "dosya değişikliği yok".to_string();
    }
    if files.len() > 4 {
        files_part.push_str(&format!(" (+{})", files.len() - 4));
    }
    rows.push(format!(
        "- {} | {} | {files_part}",
        Utc::now().format("%Y-%m-%d %H:%M"),
        clip(goal, 80)
    ));
    let skip = rows.len().saturating_sub(RECENT_SESSION_ROWS);
    rows[skip..].join("\n")
}

fn ensure_changelog(root: &Path, result: &mut DocsResult) {
    let path = changelog_path(root);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    result.changelog = name.clone();
    // Beklenen kararlı durum (repo CHANGELOG.md taşır): yalnız oluşturmada açıklanır.
    if name != CHANGELOG_NAME && !path.exists() {
        result.notes.push(format!(
            "'{name}' kullanılıyor: NTFS büyük/küçük harf duyarsız, mevcut CHANGELOG.md korunuyor"
        ));
    }
    if path.exists() {
        result.skipped.push(name);
        return;
    }
    if write_text(root, &path, &redact_text(&changelog_skeleton(root))) {
        result.created.push(name);
    }
}

// ------------------------------------------------------------ düzenleme

/// Etkin çalışma alanı için `agent.md` + `changelog.md` oluşturur/yeniler.
pub fn ensure_project_docs(root: &Path, config: &Config, opts: &DocsOptions) -> DocsResult {
    let root = resolve_path(root);
    let mut result = DocsResult::new(&root);
    let (enabled, reason) = docs_enabled(&root, config);
    if !enabled {
        result.disabled = true;
        result.reason = reason;
        return result;
    }
    if opts.read_only {
        result
            .notes
            .push("salt-okuma kipi: bağlam dosyaları okundu, yazılmadı".to_string());
        let path = agent_doc_path(&root);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        result.agent_doc = name.clone();
        result.skipped.push(name);
        return result;
    }
    if !acquire_lock(&root) {
        result
            .notes
            .push("başka bir EVREN oturumu bağlam dosyalarını güncelliyor; atlandı".to_string());
        return result;
    }
    // Önce kaydedilir ki yeni oluşturulan agent.md bu oturumun hedefini göstersin.
    if !opts.goal.trim().is_empty() {
        record_session(&root, &opts.goal, &opts.status);
    }
    ensure_agent_doc(&root, config, opts, &mut result);
    ensure_changelog(&root, &mut result);
    record_session(&root, &opts.goal, &opts.status);
    release_lock(&root);
    result
}

fn record_session(root: &Path, goal: &str, status: &str) {
    let mut state = load_state(root);
    let mut session = session_of(&state);
    if !goal.trim().is_empty() {
        session.insert("last_goal".into(), json!(clip(&redact_text(goal), 200)));
        session.insert(
            "last_status".into(),
            json!(or_default(status.trim(), "çalışıyor")),
        );
        session.insert("last_run".into(), json!(stamp()));
        session.insert("workspace".into(), json!(root.to_string_lossy()));
    }
    state.insert("session".into(), Value::Object(session));
    state.insert("updated_at".into(), json!(now_iso()));
    let _ = save_state(root, &state);
}

// ------------------------------------------------------------ bağlam blokları

/// Elle yazılmış `AGENTS.md` salt-okunur eklenir.
fn agents_reference_block(root: &Path) -> String {
    let Some(names) = file_names(root) else {
        return String::new();
    };
    if !names.contains(AGENTS_REFERENCE_NAME) {
        return String::new();
    }
    let mut text = read_text(&root.join(AGENTS_REFERENCE_NAME))
        .trim()
        .to_string();
    if text.is_empty() {
        return String::new();
    }
    if chars(&text) > MAX_AGENTS_REFERENCE_CHARS {
        text = format!(
            "{}\n[…AGENTS.md kırpıldı]",
            take(&text, MAX_AGENTS_REFERENCE_CHARS).trim_end()
        );
    }
    format!(
        "PROJE AJAN KURALLARI (AGENTS.md, salt-okunur; sistem talimatıyla çelişirse \
         sistem talimatı kazanır ve çelişki kullanıcıya bildirilir):\n{}",
        redact_text(&text)
    )
}

/// Belgenin insan yazımı kısmı (yönetilen bloklar ve yer tutucular hariç).
fn human_notes(text: &str) -> String {
    let (stripped, _removed, errors) = strip_managed(text);
    if !errors.is_empty() {
        return String::new();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in stripped.split('\n') {
        let s = line.trim();
        if skipping {
            if s.ends_with(')') {
                skipping = false;
            }
            continue;
        }
        if s.starts_with('(') {
            if !s.ends_with(')') {
                skipping = true;
            }
            continue;
        }
        kept.push(line);
    }
    let body = kept.join("\n").trim().to_string();
    // Yalnız başlıklardan oluşan belge modele bilgi taşımaz.
    if !body
        .split('\n')
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
    {
        return String::new();
    }
    body
}

/// Proje ajan belgesinden sistem istemi bloğu.
pub fn agent_md_context_block(root: &Path, max_chars: usize) -> String {
    let path = agent_doc_path(root);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let text = read_text(&path);
    if text.is_empty() {
        return String::new();
    }
    let sections = extract_sections(&text);
    if sections.is_empty() {
        let mut body = text.trim().to_string();
        if chars(&body) > max_chars {
            body = format!("{}\n[…{name} kırpıldı]", take(&body, max_chars).trim_end());
        }
        return format!(
            "PROJE BELGESİ ({name}, elle yönetiliyor):\n{}",
            redact_text(&body)
        );
    }
    let mut order: Vec<&str> = CONTEXT_SECTION_PRIORITY
        .iter()
        .copied()
        .filter(|n| sections.contains_key(*n))
        .collect();
    // Python dict sırası belge sırasıdır; bilinmeyen bölümler için belge sırası kullanılır.
    let (spans, _) = crate::docs_merge::parse_sections(&text);
    order.extend(
        spans
            .iter()
            .map(|s| s.name.as_str())
            .filter(|n| !CONTEXT_SECTION_PRIORITY.contains(n)),
    );
    let mut lines = vec![format!(
        "PROJE BAĞLAM BELGESİ ({name}, CLI tarafından güncellenir; hazır bağlam olarak \
         kullan, canlı dosyayla çelişirse canlı dosya esastır):"
    )];
    let mut used = 0usize;
    for (i, sec) in order.iter().enumerate() {
        let chunk = format!(
            "[{sec}]\n{}",
            sections.get(*sec).map(String::as_str).unwrap_or("")
        )
        .trim()
        .to_string();
        if used + chars(&chunk) > max_chars {
            lines.push(format!(
                "[…{name} kırpıldı: kalan {} bölüm]",
                order.len() - i
            ));
            break;
        }
        used += chars(&chunk);
        lines.push(chunk);
    }
    // Kullanıcının kendi bölümleri (Amaç / Notlar) üretilen blokların ardından gelir.
    let notes = human_notes(&text);
    if !notes.is_empty() && used + chars(&notes) <= max_chars {
        lines.push(format!("[{name} içindeki elle bölümler]\n{notes}"));
    } else if !notes.is_empty() {
        let room = max_chars.saturating_sub(used + 60);
        if room > 80 {
            lines.push(format!(
                "[{name} içindeki elle bölümler]\n{}\n[…elle bölümler kırpıldı: {} kr]",
                take(&notes, room).trim_end(),
                chars(&notes) - room
            ));
        }
    }
    redact_text(&lines.join("\n"))
}

/// `changelog.md` içinden "son oturumlar" bloğu.
pub fn changelog_context_block(root: &Path, max_chars: usize) -> String {
    let text = read_text(&changelog_path(root));
    if text.is_empty() {
        return String::new();
    }
    let mut recent = extract_sections(&text)
        .remove("recent")
        .unwrap_or_default()
        .trim()
        .to_string();
    if recent.is_empty() || recent.contains("henüz kayıt yok") {
        return String::new();
    }
    if chars(&recent) > max_chars {
        let tail = take_last(&recent, max_chars);
        recent = format!(
            "[…eski girdiler kırpıldı]\n{}",
            tail.trim_start_matches(['-', '\n', ' '])
        );
    }
    format!(
        "PROJE DEĞİŞİKLİK GÜNLÜĞÜ (son işler; geçmiş kayıttır, yeniden doğrulamaya \
         gerek yok):\n{}",
        redact_text(&recent)
    )
}

/// agent.md + changelog.md + AGENTS.md; token bütçesi içinde.
pub fn docs_context_block(root: &Path, config: &Config, budget_tokens: Option<i64>) -> String {
    let root = resolve_path(root);
    let budget = budget_tokens.unwrap_or_else(|| docs_token_budget(config));
    if budget <= 0 {
        return String::new();
    }
    // ~4 karakter/token; changelog küçük payı alır.
    let total = budget * 4;
    let changelog_chars = (total / 5).clamp(300, 1500);
    let agent_chars = (total - changelog_chars - 1200).max(800);
    let blocks: Vec<String> = [
        agents_reference_block(&root),
        agent_md_context_block(&root, agent_chars as usize),
        changelog_context_block(&root, changelog_chars as usize),
    ]
    .into_iter()
    .filter(|b| !b.is_empty())
    .collect();
    blocks.join("\n\n")
}

/// Netleştirme çağrısı için çok kısa proje özeti.
pub fn docs_digest(root: &Path, max_chars: usize) -> String {
    let root = resolve_path(root);
    let sections = extract_sections(&read_text(&agent_doc_path(&root)));
    if sections.is_empty() {
        return String::new();
    }
    let mut lines = vec![format!("Proje: {} ({})", root_title(&root), root.display())];
    for name in ["runtime_env", "project_map", "conventions"] {
        for line in sections
            .get(name)
            .map(String::as_str)
            .unwrap_or("")
            .split('\n')
        {
            let line = line.trim_matches(['-', ' ']).trim();
            if !line.is_empty() {
                lines.push(line.to_string());
            }
        }
    }
    let recent = extract_sections(&read_text(&changelog_path(&root)))
        .remove("recent")
        .unwrap_or_default();
    let rows: Vec<&str> = recent
        .split('\n')
        .filter(|l| l.trim().starts_with('-'))
        .collect();
    let tail: Vec<String> = rows[rows.len().saturating_sub(3)..]
        .iter()
        .map(|l| clip(l.trim_start_matches(['-', ' ']), 90))
        .collect();
    if !tail.is_empty() {
        lines.push(format!("Son işler: {}", tail.join(" / ")));
    }
    let mut digest = lines.join("\n");
    if chars(&digest) > max_chars {
        digest = format!("{}…", take(&digest, max_chars).trim_end());
    }
    redact_text(&digest)
}

// ------------------------------------------------------------ bakım (`/docs`, `evren docs`)

fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".evren-write-probe");
    if std::fs::write(&probe, "ok").is_err() {
        return false;
    }
    let _ = std::fs::remove_file(probe);
    true
}

/// CLI'nin kendi bağlam dosyalarının (skills.md, lessons.md, commands.md)
/// yazılabilir ilk dizini: `EVREN_CLI_HOME`, sonra `~/.evren/cli_home`.
/// Python'daki kaynak ağacı adayı derlenmiş ikilide yoktur.
pub fn cli_docs_dir(config: &Config) -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let raw = config
        .get("EVREN_CLI_HOME")
        .unwrap_or("")
        .trim()
        .to_string();
    if !raw.is_empty() {
        let expanded = match raw.strip_prefix('~') {
            Some(rest) => dirs::home_dir()
                .map(|h| h.join(rest.trim_start_matches(['/', '\\'])))
                .unwrap_or_else(|| PathBuf::from(&raw)),
            None => PathBuf::from(&raw),
        };
        candidates.push(expanded);
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".evren").join("cli_home"));
    }
    for c in candidates {
        if is_writable(&c) {
            return resolve_path(&c);
        }
    }
    resolve_path(&std::env::temp_dir())
}

/// `evren docs status` / `/docs status` raporu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocsStatus {
    pub root: String,
    pub enabled: bool,
    pub reason: String,
    pub agent_doc: String,
    pub agent_sections: Vec<String>,
    pub changelog: String,
    pub changelog_entries: usize,
    pub state: String,
    pub token_budget: i64,
}

pub fn docs_status(root: &Path, config: &Config) -> DocsStatus {
    let root = resolve_path(root);
    let (enabled, reason) = docs_enabled(&root, config);
    let path = agent_doc_path(&root);
    let agent_sections: Vec<String> = if path.exists() {
        extract_sections(&read_text(&path)).into_keys().collect()
    } else {
        Vec::new()
    };
    let changelog = changelog_path(&root);
    let state = state_path(&root);
    DocsStatus {
        root: root.to_string_lossy().to_string(),
        enabled,
        reason,
        agent_doc: if path.exists() {
            path.to_string_lossy().to_string()
        } else {
            String::new()
        },
        agent_sections,
        changelog: if changelog.exists() {
            changelog.to_string_lossy().to_string()
        } else {
            String::new()
        },
        changelog_entries: read_text(&changelog).matches("\n## ").count(),
        state: if state.exists() {
            state.to_string_lossy().to_string()
        } else {
            String::new()
        },
        token_budget: docs_token_budget(config),
    }
}

/// Yönetilen blokları kaldırır (insan metni kalır); `/docs unmark`.
pub fn unmark_docs(root: &Path) -> DocsResult {
    let root = resolve_path(root);
    let mut result = DocsResult::new(&root);
    let names = [
        agent_doc_path(&root)
            .file_name()
            .map(|n| n.to_string_lossy().to_string()),
        changelog_path(&root)
            .file_name()
            .map(|n| n.to_string_lossy().to_string()),
    ];
    for name in names.into_iter().flatten() {
        let path = root.join(&name);
        if !path.exists() {
            continue;
        }
        let (new_text, removed, errors) = strip_managed(&read_text(&path));
        if let Some(err) = errors.first() {
            result.notes.push(format!("{name}: {err} (dokunulmadı)"));
            continue;
        }
        if removed.is_empty() {
            result.skipped.push(name);
            continue;
        }
        if write_text(&root, &path, &new_text) {
            result.updated.push(format!(
                "{name} (yönetilen bloklar kaldırıldı: {})",
                removed.join(", ")
            ));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    //! Python `tests/test_project_docs.py`.
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Config {
        let mut env: BTreeMap<String, String> =
            BTreeMap::from([("EVREN_AUTO_DOCS".to_string(), "true".to_string())]);
        for (k, v) in pairs {
            env.insert(k.to_string(), v.to_string());
        }
        Config::from_env(env)
    }

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = resolve_path(dir.path());
        (dir, root)
    }

    fn read(root: &Path, name: &str) -> String {
        read_text(&root.join(name))
    }

    fn write(root: &Path, name: &str, text: &str) {
        let _ = std::fs::write(root.join(name), text);
    }

    fn run(root: &Path, config: &Config, opts: DocsOptions) -> DocsResult {
        ensure_project_docs(root, config, &opts)
    }

    #[test]
    fn creates_agent_and_changelog() {
        let (_d, root) = tmp();
        let r = run(&root, &cfg(&[]), DocsOptions::default());
        assert!(!r.disabled);
        assert!(r.created.contains(&"agent.md".to_string()));
        assert!(r.created.contains(&"changelog.md".to_string()));
        assert!(root.join("agent.md").exists());
        assert!(root.join("changelog.md").exists());
        assert_eq!(r.summary(), "hazır: agent.md, changelog.md");
        let text = read(&root, "agent.md");
        for name in AGENT_SECTIONS {
            assert!(text.contains(&format!("section={name} -->")), "{name}");
        }
        assert!(text.contains("## Notlar (elle)"));
    }

    #[test]
    fn second_run_writes_nothing() {
        let (_d, root) = tmp();
        let c = cfg(&[]);
        run(&root, &c, DocsOptions::default());
        let before = std::fs::read(root.join("agent.md")).unwrap_or_default();
        let r = run(&root, &c, DocsOptions::default());
        assert_eq!(
            std::fs::read(root.join("agent.md")).unwrap_or_default(),
            before
        );
        assert!(r.updated.is_empty());
        assert!(r.skipped.contains(&"agent.md".to_string()));
        assert_eq!(r.summary(), "güncel");
    }

    #[test]
    fn human_sections_survive_refresh() {
        let (_d, root) = tmp();
        let c = cfg(&[]);
        run(&root, &c, DocsOptions::default());
        let text = read(&root, "agent.md") + "\n## Benim kurallarım\nAsla force-push yapma.\n";
        write(&root, "agent.md", &text);
        let opts = DocsOptions {
            goal: "ikinci görev".into(),
            ..Default::default()
        };
        let r = run(&root, &c, opts);
        let after = read(&root, "agent.md");
        assert!(after.contains("Asla force-push yapma."));
        assert!(after.contains("- Son görev: ikinci görev"));
        assert!(r.updated.contains(&"agent.md#current_focus".to_string()));
    }

    #[test]
    fn unmanaged_document_is_left_alone() {
        let (_d, root) = tmp();
        let original = "# Bizim agent.md\n\nElle yazılmış kurallar.\n";
        write(&root, "agent.md", original);
        let r = run(&root, &cfg(&[]), DocsOptions::default());
        assert_eq!(read(&root, "agent.md"), original);
        assert!(r.skipped.contains(&"agent.md".to_string()));
        assert!(r.notes.iter().any(|n| n.contains("--adopt")));
        // --adopt bölümleri ekler, elle metin kalır.
        let r = run(
            &root,
            &cfg(&[]),
            DocsOptions {
                refresh: true,
                adopt: true,
                ..Default::default()
            },
        );
        let text = read(&root, "agent.md");
        assert!(text.starts_with(original.trim_end()));
        assert!(text.contains("section=identity"));
        assert!(r.updated.iter().any(|u| u.starts_with("agent.md#")));
    }

    #[test]
    fn agents_md_untouched_and_both_in_context() {
        let (_d, root) = tmp();
        write(&root, "AGENTS.md", "# AGENTS.md\n\nProje kuralları.\n");
        let c = cfg(&[]);
        let r = run(&root, &c, DocsOptions::default());
        assert_eq!(
            read(&root, "AGENTS.md"),
            "# AGENTS.md\n\nProje kuralları.\n"
        );
        let names = file_names(&root).unwrap_or_default();
        if names.contains("agent.md") {
            // Büyük/küçük harf duyarlı dosya sistemi.
            assert_eq!(r.conflict, "");
        } else {
            // NTFS: alternatif ad seçilir, AGENTS.md korunur.
            assert!(names.contains(AGENT_DOC_ALT_NAME));
            assert!(r.conflict.contains("AGENTS.md"));
        }
        let block = docs_context_block(&root, &c, None);
        assert!(block.contains("Proje kuralları."));
        assert!(block.contains("[runtime_env]"));
    }

    #[test]
    fn output_safety() {
        let (_d, root) = tmp();
        let opts = DocsOptions {
            goal: "evren_llm_GIZLI123 ile giriş".into(),
            ..Default::default()
        };
        run(&root, &cfg(&[]), opts);
        let text = read(&root, "agent.md");
        assert!(!text.contains("evren_llm_GIZLI123"));
        assert!(text.contains("[REDACTED]"));

        let (_d2, root2) = tmp();
        let r = run(
            &root2,
            &cfg(&[("EVREN_AUTO_DOCS", "false")]),
            DocsOptions::default(),
        );
        assert!(r.disabled);
        assert!(!root2.join("agent.md").exists());
        assert_eq!(r.summary(), "kapalı (EVREN_AUTO_DOCS=false)");

        let (_d3, root3) = tmp();
        let r = run(
            &root3,
            &cfg(&[]),
            DocsOptions {
                read_only: true,
                ..Default::default()
            },
        );
        assert!(!root3.join("agent.md").exists());
        assert!(r.notes.iter().any(|n| n.contains("salt-okuma")));
    }

    #[test]
    fn docs_json_disables() {
        let (_d, root) = tmp();
        let _ = std::fs::create_dir_all(root.join(".evren"));
        write(&root, ".evren/docs.json", "{\"disabled\": true}");
        let (on, reason) = docs_enabled(&root, &cfg(&[]));
        assert!(!on);
        assert_eq!(reason, ".evren/docs.json disabled=true");
    }

    #[test]
    fn changelog_entry_appended_and_recent_updated() {
        let (_d, root) = tmp();
        let c = cfg(&[]);
        run(&root, &c, DocsOptions::default());
        let entry = json!({"goal": "docs özelliği", "files": ["src/a.py"], "exit_codes": [0],
                           "verification": "pytest 3 passed"});
        assert!(append_changelog_entry(&root, &c, &entry));
        let text = read(&root, "changelog.md");
        assert!(text.contains("docs özelliği"));
        assert!(text.contains("`src/a.py`"));
        assert!(text.contains("pytest 3 passed"));
        let recent = extract_sections(&text).remove("recent").unwrap_or_default();
        assert!(recent.contains("docs özelliği"));
        assert!(!recent.contains("henüz kayıt yok"));
        // Aynı girdi ikinci kez yazılmaz.
        assert!(!append_changelog_entry(&root, &c, &entry));
        let block = changelog_context_block(&root, 1500);
        assert!(block.contains("docs özelliği"));
    }

    #[test]
    fn changelog_alt_name_with_upper_case_sibling() {
        let (_d, root) = tmp();
        write(&root, "CHANGELOG.md", "# İnsan changelog\n");
        let names = file_names(&root).unwrap_or_default();
        if names.contains("changelog.md") {
            return; // büyük/küçük harf duyarlı dosya sistemi değil
        }
        assert_eq!(changelog_path(&root), root.join(CHANGELOG_ALT_NAME));
    }

    #[test]
    fn token_budget_zero_disables_injection() {
        let (_d, root) = tmp();
        let c = cfg(&[]);
        run(&root, &c, DocsOptions::default());
        assert_eq!(docs_context_block(&root, &c, Some(0)), "");
        assert_eq!(
            docs_context_block(&root, &cfg(&[("EVREN_DOCS_TOKENS", "0")]), None),
            ""
        );
        assert!(!docs_context_block(&root, &c, None).is_empty());
    }

    #[test]
    fn unmark_removes_managed_blocks() {
        let (_d, root) = tmp();
        let c = cfg(&[]);
        run(&root, &c, DocsOptions::default());
        let r = unmark_docs(&root);
        assert_eq!(r.updated.len(), 2);
        assert!(!read(&root, "agent.md").contains("evren:managed"));
        assert!(read(&root, "agent.md").contains("## Notlar (elle)"));
        let st = docs_status(&root, &c);
        assert!(st.agent_sections.is_empty());
    }

    #[test]
    fn clean_goal_strips_frame_template() {
        assert_eq!(
            clean_goal("GÖREV: Brief: testleri düzelt."),
            "testleri düzelt"
        );
        assert_eq!(
            clean_goal("Rol x. Çıktı, format ve teslim yüzeyidir. Brief: README yaz"),
            "README yaz"
        );
        assert_eq!(clean_goal("düz görev"), "düz görev");
    }

    #[test]
    fn digest_and_human_notes() {
        let (_d, root) = tmp();
        let c = cfg(&[]);
        run(&root, &c, DocsOptions::default());
        let d = docs_digest(&root, 1200);
        assert!(d.starts_with("Proje: "));
        // Şablondaki yer tutucular bağlama girmez.
        assert_eq!(human_notes(&read(&root, "agent.md")), "");
        let text = read(&root, "agent.md") + "\nAsla force-push yapma.\n";
        assert!(human_notes(&text).contains("Asla force-push yapma."));
    }
}
