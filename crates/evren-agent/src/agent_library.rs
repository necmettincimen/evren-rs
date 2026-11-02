//! Ajan kitaplığı: front-matter'lı markdown ajan tanımları (Python `agent_library.py`).
//!
//! Yükleme sırası (aynı adda sonraki öncekini ezer): yerleşik `agents/*.md`
//! (ikiliye gömülü), kullanıcı `<cli_docs_dir>/agents/`, proje
//! `<çalışma alanı>/.evren/agents/`.

use std::collections::{BTreeMap, HashMap};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use evren_core::config::Config;
use regex::Regex;
use serde_json::{Map, Value};

pub const SOURCES: [&str; 3] = ["yerleşik", "kullanıcı", "proje"];

const BUILTIN: [(&str, &str); 5] = [
    ("devops.md", include_str!("../agents/devops.md")),
    ("fullstack.md", include_str!("../agents/fullstack.md")),
    (
        "product-owner.md",
        include_str!("../agents/product-owner.md"),
    ),
    ("researcher.md", include_str!("../agents/researcher.md")),
    ("tester.md", include_str!("../agents/tester.md")),
];

pub const AGENT_TEMPLATE: &str = "---
name: {name}
description: Bu ajanın görevi (tek satır)
tools: [view_file, find_files, list_directory, run_command, todo_write]
model:
thinking: false
max_steps: 15
---
Sen {name} ajanısın. Rolünü ve çalışma kurallarını burada yaz.
";

#[derive(Debug, Clone, PartialEq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub tools: Option<Vec<String>>,
    pub model: String,
    pub thinking: bool,
    pub max_steps: u32,
    pub prompt: String,
    pub source: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
enum Scalar {
    List(Vec<String>),
    Bool(bool),
    Str(String),
}

impl Scalar {
    fn text(&self) -> String {
        match self {
            Scalar::Str(s) => s.clone(),
            Scalar::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            Scalar::List(items) => {
                let quoted: Vec<String> = items.iter().map(|i| format!("'{i}'")).collect();
                format!("[{}]", quoted.join(", "))
            }
        }
    }

    /// Python doğruluk değeri (boş dize/liste ve False yanlıştır).
    fn truthy(&self) -> bool {
        match self {
            Scalar::Str(s) => !s.is_empty(),
            Scalar::Bool(b) => *b,
            Scalar::List(items) => !items.is_empty(),
        }
    }
}

fn strip_quotes(s: &str) -> &str {
    s.trim_matches(|c| c == '\'' || c == '"')
}

fn scalar(value: &str) -> Scalar {
    let value = value.trim();
    if value.starts_with('[') && value.ends_with(']') && value.len() >= 2 {
        let inner = value[1..value.len() - 1].trim();
        return Scalar::List(
            inner
                .split(',')
                .filter(|p| !p.trim().is_empty())
                .map(|p| strip_quotes(p.trim()).to_string())
                .collect(),
        );
    }
    match value.to_lowercase().as_str() {
        "true" | "evet" => Scalar::Bool(true),
        "false" | "hayır" | "hayir" => Scalar::Bool(false),
        _ => Scalar::Str(strip_quotes(value).to_string()),
    }
}

fn name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][\w-]*$").unwrap_or_else(|_| unreachable!()))
}

fn comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+#.*$").unwrap_or_else(|_| unreachable!()))
}

/// Geçerli ajan adı mı (`^[A-Za-z0-9][\w-]*$`).
pub fn valid_name(name: &str) -> bool {
    name_re().is_match(name)
}

fn py_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

type Meta = HashMap<String, Scalar>;

/// `---` front-matter'ı (basit YAML alt kümesi) gövdeden ayırır.
fn parse_agent_text(text: &str) -> Result<(Meta, String), String> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    if !text.starts_with("---") {
        return Err("front-matter yok".into());
    }
    let Some((head, rest)) = text.split_once("\n---") else {
        return Err("front-matter kapanmıyor".into());
    };
    let mut meta = Meta::new();
    for line in head.lines().skip(1) {
        let line = comment_re().replace(line, "");
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, val)) = line.split_once(':') else {
            return Err(format!("geçersiz satır: {}", py_repr(line)));
        };
        meta.insert(key.trim().to_lowercase(), scalar(val));
    }
    let body = rest.split_once('\n').map(|(_, b)| b).unwrap_or("");
    Ok((meta, body.trim().to_string()))
}

fn parse_agent(text: &str, stem: &str, source: &str, path: &str) -> Result<AgentDef, String> {
    let (meta, body) = parse_agent_text(text)?;
    let name = match meta.get("name") {
        Some(v) if v.truthy() => v.text(),
        _ => stem.to_string(),
    };
    let name = name.trim().to_string();
    if !valid_name(&name) {
        return Err(format!("geçersiz ad: {}", py_repr(&name)));
    }
    let tools = match meta.get("tools") {
        Some(Scalar::List(items)) => Some(items.clone()),
        Some(Scalar::Str(s)) => {
            let items: Vec<String> = s
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect();
            (!items.is_empty()).then_some(items)
        }
        _ => None,
    };
    let steps: i64 = match meta.get("max_steps") {
        Some(Scalar::Str(s)) if !s.is_empty() => s
            .trim()
            .parse()
            .map_err(|_| "max_steps sayı olmalı".to_string())?,
        Some(Scalar::Bool(true)) => 1,
        Some(Scalar::List(items)) if !items.is_empty() => {
            return Err("max_steps sayı olmalı".into());
        }
        _ => 15,
    };
    if body.is_empty() {
        return Err("talimat gövdesi boş".into());
    }
    let text_of = |key: &str| match meta.get(key) {
        Some(v) if v.truthy() => v.text(),
        _ => String::new(),
    };
    Ok(AgentDef {
        name,
        description: text_of("description"),
        tools,
        model: text_of("model"),
        thinking: meta.get("thinking") == Some(&Scalar::Bool(true)),
        max_steps: steps.clamp(1, 60) as u32,
        prompt: body,
        source: source.to_string(),
        path: path.to_string(),
    })
}

fn warn(msg: &str) {
    if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        println!("\x1b[33m⚠\x1b[0m {msg}");
    } else {
        println!("[!] {msg}");
    }
}

/// Kullanıcı ve proje ajan dizinleri (yerleşikler ikiliye gömülüdür).
pub fn agent_dirs(workspace_root: &Path, config: &Config) -> Vec<(&'static str, PathBuf)> {
    vec![
        (
            "kullanıcı",
            evren_core::project_docs::cli_docs_dir(config).join("agents"),
        ),
        ("proje", workspace_root.join(".evren").join("agents")),
    ]
}

fn md_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    files.sort();
    files
}

type Stamp = Vec<(PathBuf, Option<SystemTime>, u64)>;
type AgentMap = BTreeMap<String, AgentDef>;

fn dirs_stamp(dirs: &[(&str, PathBuf)]) -> Stamp {
    let mut stamp = Stamp::new();
    for (_, dir) in dirs {
        for path in md_files(dir) {
            let meta = std::fs::metadata(&path).ok();
            let mtime = meta.as_ref().and_then(|m| m.modified().ok());
            let len = meta.map(|m| m.len()).unwrap_or(0);
            stamp.push((path, mtime, len));
        }
    }
    stamp
}

fn load_uncached(dirs: &[(&str, PathBuf)]) -> AgentMap {
    let mut agents = AgentMap::new();
    for (file, text) in BUILTIN {
        let stem = file.trim_end_matches(".md");
        match parse_agent(text, stem, "yerleşik", file) {
            Ok(a) => {
                agents.insert(a.name.clone(), a);
            }
            Err(e) => warn(&format!("Ajan dosyası atlandı ({file}): {e}")),
        }
    }
    for (source, dir) in dirs {
        for path in md_files(dir) {
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let stem = path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| parse_agent(&t, &stem, source, &path.to_string_lossy()));
            match parsed {
                Ok(a) => {
                    agents.insert(a.name.clone(), a);
                }
                Err(e) => warn(&format!("Ajan dosyası atlandı ({file}): {e}")),
            }
        }
    }
    agents
}

/// Tüm ajan tanımları; yalnız bir `agents/*.md` değişince yeniden ayrıştırılır.
pub fn load_agents(workspace_root: &Path, config: &Config) -> AgentMap {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Stamp, AgentMap)>>> = OnceLock::new();
    let dirs = agent_dirs(workspace_root, config);
    let stamp = dirs_stamp(&dirs);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = workspace_root.to_path_buf();
    if let Ok(guard) = cache.lock() {
        if let Some((s, agents)) = guard.get(&key) {
            if *s == stamp {
                return agents.clone();
            }
        }
    }
    let agents = load_uncached(&dirs);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, (stamp, agents.clone()));
    }
    agents
}

pub fn get_agent(name: &str, workspace_root: &Path, config: &Config) -> Option<AgentDef> {
    let key = name.trim().trim_start_matches('@');
    load_agents(workspace_root, config).remove(key)
}

fn settings_path(workspace_root: &Path) -> PathBuf {
    evren_core::config::get_evren_dir(workspace_root).join("settings.json")
}

/// `.evren/settings.json` (yoksa/bozuksa boş nesne).
pub fn load_settings(workspace_root: &Path) -> Map<String, Value> {
    std::fs::read_to_string(settings_path(workspace_root))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(t.trim_start_matches('\u{feff}')).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

/// Ayarları geçici dosya + yeniden adlandırma ile yazar.
pub fn save_settings(workspace_root: &Path, settings: &Map<String, Value>) -> std::io::Result<()> {
    let path = settings_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(settings.clone()))
        .map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// Oturumun ana ajanı (`settings.json` → `agent`).
pub fn active_agent(workspace_root: &Path, config: &Config) -> Option<AgentDef> {
    let name = match load_settings(workspace_root).get("agent") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | Some(Value::Bool(false)) | None => String::new(),
        Some(other) => other.to_string(),
    };
    if name.is_empty() {
        return None;
    }
    get_agent(&name, workspace_root, config)
}

/// Ana ajanın sistem bloğu (ayarlı değilse boş).
pub fn agent_context_block(workspace_root: &Path, config: &Config) -> String {
    active_agent(workspace_root, config)
        .map(|a| format!("ANA AJAN: {}\n{}", a.name, a.prompt))
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse() {
        let dir = tempfile::tempdir().unwrap();
        let agents = load_uncached(&[("proje", dir.path().join("yok"))]);
        let names: Vec<&str> = agents.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "devops",
                "fullstack",
                "product-owner",
                "researcher",
                "tester"
            ]
        );
        let r = &agents["researcher"];
        assert_eq!(r.max_steps, 8);
        assert_eq!(r.source, "yerleşik");
        assert!(r.tools.as_ref().unwrap().contains(&"web_fetch".to_string()));
        assert!(!r.thinking);
    }

    #[test]
    fn project_overrides_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("tester.md"),
            "---\nname: tester\ndescription: \"yeni\"  # yorum\ntools: a, b\nthinking: evet\nmax_steps: 99\n---\nGövde\n",
        )
        .unwrap();
        std::fs::write(agents_dir.join("bozuk.md"), "gövde yok").unwrap();
        let agents = load_uncached(&[("proje", agents_dir)]);
        let t = &agents["tester"];
        assert_eq!(t.description, "yeni");
        assert_eq!(t.tools, Some(vec!["a".to_string(), "b".to_string()]));
        assert!(t.thinking);
        assert_eq!(t.max_steps, 60);
        assert_eq!(t.source, "proje");
        assert!(!agents.contains_key("bozuk"));
    }

    #[test]
    fn parse_errors() {
        assert_eq!(
            parse_agent("x", "a", "proje", "").unwrap_err(),
            "front-matter yok"
        );
        assert_eq!(
            parse_agent("---\nname: a\n", "a", "proje", "").unwrap_err(),
            "front-matter kapanmıyor"
        );
        assert_eq!(
            parse_agent("---\nbozuk\n---\nx", "a", "proje", "").unwrap_err(),
            "geçersiz satır: 'bozuk'"
        );
        assert_eq!(
            parse_agent("---\nname: -x\n---\nx", "a", "proje", "").unwrap_err(),
            "geçersiz ad: '-x'"
        );
        assert_eq!(
            parse_agent("---\nmax_steps: on\n---\nx", "a", "proje", "").unwrap_err(),
            "max_steps sayı olmalı"
        );
        assert_eq!(
            parse_agent("---\nname: a\n---\n  \n", "a", "proje", "").unwrap_err(),
            "talimat gövdesi boş"
        );
    }

    #[test]
    fn template_parses() {
        let text = AGENT_TEMPLATE.replace("{name}", "yardimci");
        let a = parse_agent(&text, "yardimci", "proje", "").unwrap();
        assert_eq!(a.name, "yardimci");
        assert_eq!(a.model, "");
        assert_eq!(a.max_steps, 15);
        assert_eq!(a.tools.unwrap().len(), 5);
    }
}
