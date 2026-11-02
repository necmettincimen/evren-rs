//! Yapılandırma (`config.py`). Ortam değişkenleri + ilk bulunan `.env` +
//! eski `~/.evren-cli/config.json`. Python modül yükleme anında okurdu;
//! burada `Config::load()` aynı işi yapar ve değerleri bir haritada tutar,
//! böylece testler süreç ortamına dokunmadan çalışır.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::EvrenError;
use crate::models::{self, Role};
use crate::texts;

pub const DEFAULT_BASE_URL: &str = "https://evren-llmapi.ssyz.org.tr/v1";
pub const DEFAULT_MODEL: &str = "mimo-v2.6-pro";
pub const DEFAULT_FALLBACK_MODEL: &str = "qwen3.8-flash-next";
pub const DEFAULT_SUMMARY_MODEL: &str = "qwen3.8-flash-next";
pub const DEFAULT_CONTEXT_BUDGET: i64 = 64_000;
pub const DEFAULT_MAX_TOKENS: u32 = 4096;
pub const DEFAULT_TEMPERATURE: f64 = 0.3;
pub const DEFAULT_MAX_STEPS: u32 = 30;
pub const API_KEY_PREFIX: &str = "evren_llm_";

/// `.evren/.gitignore` içinde bulunması gereken satırlar.
pub const EVREN_GITIGNORE_ENTRIES: [&str; 8] = [
    "backups/",
    "logs/",
    "history.json",
    "checkpoint.json",
    "decisions.json",
    "docs_state.json",
    "docs.lock",
    "tmp/",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    ProcessEnv,
    LegacyConfig,
    EnvFile,
    NotFound,
}

#[derive(Debug, Clone)]
pub struct Config {
    env: BTreeMap<String, String>,
    pub env_file: Option<PathBuf>,
    pub env_candidates: Vec<PathBuf>,
    pub legacy_config_path: PathBuf,
    pub legacy_keys_used: usize,
    pub preexisting_key_env: bool,
}

impl Config {
    /// Süreç ortamını okur, ilk `.env` adayını ve eski config.json'u uygular.
    pub fn load() -> Self {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = home_dir();
        Self::from_parts(env, &cwd, &home)
    }

    /// Yalnız verilen haritayla, dosya okumadan (testler için).
    pub fn from_env(env: BTreeMap<String, String>) -> Self {
        Config {
            preexisting_key_env: has_key_env(&env),
            env,
            env_file: None,
            env_candidates: Vec::new(),
            legacy_config_path: PathBuf::from(".evren-cli/config.json"),
            legacy_keys_used: 0,
        }
    }

    pub fn from_parts(mut env: BTreeMap<String, String>, cwd: &Path, home: &Path) -> Self {
        let preexisting_key_env = has_key_env(&env);
        let env_candidates = vec![cwd.join(".env"), home.join(".evren").join(".env")];
        let env_file = env_candidates.iter().find(|p| p.is_file()).cloned();
        if let Some(path) = &env_file {
            load_env_file(path, &mut env);
        }
        let legacy_config_path = home.join(".evren-cli").join("config.json");
        let legacy_keys_used = apply_legacy_config(&legacy_config_path, &mut env);
        Config {
            env,
            env_file,
            env_candidates,
            legacy_config_path,
            legacy_keys_used,
            preexisting_key_env,
        }
    }

    // ------------------------------------------------------------ ham erişim

    pub fn get(&self, key: &str) -> Option<&str> {
        self.env.get(key).map(String::as_str)
    }

    fn get_or(&self, key: &str, default: &str) -> String {
        self.env
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_owned())
    }

    /// Boş → varsayılan; değilse `1|true|yes|on`.
    pub fn flag(&self, key: &str, default: bool) -> bool {
        let raw = self.get(key).unwrap_or("").trim().to_lowercase();
        if raw.is_empty() {
            return default;
        }
        matches!(raw.as_str(), "1" | "true" | "yes" | "on")
    }

    /// Ayrıştırma hatası ya da aralık dışı → varsayılan.
    pub fn int(&self, key: &str, default: i64, low: i64, high: i64) -> i64 {
        let raw = self.get(key).map(str::trim).filter(|s| !s.is_empty());
        match raw.map(str::parse::<i64>) {
            None => default,
            Some(Ok(v)) if (low..=high).contains(&v) => v,
            Some(_) => default,
        }
    }

    fn float_in(&self, key: &str, default: f64, low: f64, high: f64) -> f64 {
        match self.get(key).map(|s| s.trim().parse::<f64>()) {
            None => default,
            Some(Ok(v)) if (low..=high).contains(&v) => v,
            Some(_) => default,
        }
    }

    // ------------------------------------------------------------ anahtarlar

    pub fn api_key(&self) -> String {
        self.get_or("EVREN_API_KEY", "")
    }

    /// `api_key_pool.parse_api_keys`: EVREN_API_KEYS (virgül) + EVREN_API_KEY,
    /// sıra korunarak tekilleştirilir.
    pub fn api_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |k: &str| {
            let k = k.trim();
            if !k.is_empty() && !out.iter().any(|x| x == k) {
                out.push(k.to_owned());
            }
        };
        for k in self.get_or("EVREN_API_KEYS", "").split(',') {
            push(k);
        }
        push(&self.api_key());
        out
    }

    /// Python: anahtar `evren_llm_` ile başlıyorsa yapılandırılmış sayılır.
    pub fn check_api_key_configured(&self) -> bool {
        self.api_key().starts_with(API_KEY_PREFIX)
    }

    pub fn require_api_key(&self) -> Result<(), EvrenError> {
        if self.api_keys().is_empty() {
            return Err(EvrenError::Config(texts::API_KEY_MISSING.to_owned()));
        }
        Ok(())
    }

    pub fn key_source(&self) -> KeySource {
        if self.preexisting_key_env {
            KeySource::ProcessEnv
        } else if self.legacy_keys_used > 0 {
            KeySource::LegacyConfig
        } else if self.env_file.is_some() {
            KeySource::EnvFile
        } else {
            KeySource::NotFound
        }
    }

    /// `config_source_report`: yalnız yol, sayı ve kaynak; anahtar değeri yok.
    pub fn source_report(&self) -> Map<String, Value> {
        let pool_size = self.api_keys().len();
        let key_source = match self.key_source() {
            KeySource::ProcessEnv => texts::KEY_SOURCE_PROCESS_ENV.to_owned(),
            KeySource::LegacyConfig => format!(
                "{} (ApiKeys, {} anahtar)",
                self.legacy_config_path.display(),
                self.legacy_keys_used
            ),
            KeySource::EnvFile => format!(
                "{} (EVREN_API_KEY / EVREN_API_KEYS)",
                self.env_file
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            KeySource::NotFound => texts::KEY_SOURCE_NOT_FOUND.to_owned(),
        };
        let mut m = Map::new();
        m.insert(
            "env_file".into(),
            Value::from(
                self.env_file
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
        );
        m.insert(
            "env_candidates".into(),
            Value::from(
                self.env_candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>(),
            ),
        );
        m.insert(
            "config_json".into(),
            Value::from(self.legacy_config_path.display().to_string()),
        );
        m.insert(
            "config_json_exists".into(),
            Value::from(self.legacy_config_path.exists()),
        );
        m.insert(
            "config_json_keys".into(),
            Value::from(self.legacy_keys_used),
        );
        m.insert("key_source".into(), Value::from(key_source));
        m.insert("key_count".into(), Value::from(pool_size));
        m.insert(
            "has_key".into(),
            Value::from(!self.api_key().is_empty() || pool_size > 0),
        );
        m
    }

    // ------------------------------------------------------------ model/LLM

    pub fn base_url(&self) -> String {
        self.get_or("EVREN_BASE_URL", DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_owned()
    }

    pub fn default_model(&self) -> String {
        self.get_or("EVREN_DEFAULT_MODEL", DEFAULT_MODEL)
    }

    pub fn fallback_model(&self) -> String {
        self.get_or("EVREN_FALLBACK_MODEL", DEFAULT_FALLBACK_MODEL)
            .trim()
            .to_owned()
    }

    pub fn fallback_after(&self) -> u32 {
        self.get("EVREN_FALLBACK_AFTER")
            .and_then(|s| s.trim().parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(2)
    }

    pub fn default_max_tokens(&self) -> u32 {
        self.get("EVREN_DEFAULT_MAX_TOKENS")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(DEFAULT_MAX_TOKENS)
    }

    pub fn default_temperature(&self) -> f64 {
        self.get("EVREN_DEFAULT_TEMPERATURE")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(DEFAULT_TEMPERATURE)
    }

    pub fn auto_approve(&self) -> bool {
        matches!(
            self.get_or("EVREN_AUTO_APPROVE", "true")
                .to_lowercase()
                .as_str(),
            "true" | "1" | "yes"
        )
    }

    /// 1–100 dışı → 30.
    pub fn default_max_steps(&self) -> u32 {
        self.int("EVREN_MAX_STEPS", DEFAULT_MAX_STEPS.into(), 1, 100) as u32
    }

    pub fn compaction_ratio(&self) -> f64 {
        self.float_in("EVREN_COMPACT_RATIO", 0.6, 0.2, 1.0)
    }

    pub fn context_budget(&self) -> i64 {
        self.int(
            "EVREN_CONTEXT_BUDGET",
            DEFAULT_CONTEXT_BUDGET,
            4_000,
            2_000_000,
        )
    }

    pub fn compact_target_ratio(&self) -> f64 {
        self.float_in("EVREN_COMPACT_TARGET", 0.5, 0.3, 0.8)
    }

    pub fn smart_compact_enabled(&self) -> bool {
        self.flag("EVREN_SMART_COMPACT", true)
    }

    pub fn summary_model(&self) -> String {
        let s = self.get_or("EVREN_SUMMARY_MODEL", "");
        if s.is_empty() {
            DEFAULT_SUMMARY_MODEL.to_owned()
        } else {
            s.trim().to_owned()
        }
    }

    /// `light` | `dark`; başka değer hata.
    pub fn ui_theme(&self) -> Result<String, EvrenError> {
        let t = self.get_or("EVREN_UI_THEME", "dark").trim().to_lowercase();
        if t == "light" || t == "dark" {
            Ok(t)
        } else {
            Err(EvrenError::Config(texts::UI_THEME_INVALID.to_owned()))
        }
    }

    /// `""|default|auto|none|off` → None.
    pub fn reasoning_effort(&self) -> Option<String> {
        let e = self
            .get_or("EVREN_REASONING_EFFORT", "low")
            .trim()
            .to_lowercase();
        match e.as_str() {
            "" | "default" | "auto" | "none" | "off" => None,
            _ => Some(e),
        }
    }

    pub fn key_min_remaining(&self) -> i64 {
        self.int("EVREN_KEY_MIN_REMAINING", 50_000, 0, 15_000_000)
    }

    pub fn auto_route_enabled(&self) -> bool {
        self.flag("EVREN_AUTO_ROUTE", true)
    }

    pub fn planner_model(&self) -> String {
        let s = self.get_or("EVREN_PLANNER_MODEL", "").trim().to_owned();
        if s.is_empty() {
            models::default_model_for(Role::Planner, "deepseek-v4.1-flash")
        } else {
            s
        }
    }

    pub fn executor_model(&self, session_model: &str) -> String {
        let s = self.get_or("EVREN_EXECUTOR_MODEL", "").trim().to_owned();
        if s.is_empty() {
            session_model.to_owned()
        } else {
            s
        }
    }

    pub fn planner_max_steps(&self) -> i64 {
        self.int("EVREN_PLANNER_STEPS", 4, 1, 20)
    }

    pub fn executor_thinking(&self) -> bool {
        self.flag("EVREN_EXECUTOR_THINKING", false)
    }

    pub fn route_min_tokens(&self) -> i64 {
        self.int("EVREN_ROUTE_MIN_TOKENS", 800, 50, 1_000_000)
    }

    pub fn clef_intent_enabled(&self) -> bool {
        self.flag("EVREN_CLEF_INTENT", true)
    }

    pub fn paste_max_lines(&self) -> i64 {
        self.int("EVREN_PASTE_MAX_LINES", 80, 20, 1_000_000)
    }

    pub fn paste_max_chars(&self) -> i64 {
        self.int("EVREN_PASTE_MAX_CHARS", 6000, 1000, 100_000_000)
    }

    pub fn trace_enabled(&self) -> bool {
        let raw = self.get_or("EVREN_TRACE", "").trim().to_lowercase();
        !matches!(
            raw.as_str(),
            "false" | "0" | "off" | "no" | "hayır" | "hayir"
        )
    }

    pub fn thinking_off_supported(&self, model: &str) -> bool {
        models::thinking_off_supported(model)
    }

    // ------------------------------------------------------------ TLS

    pub fn ca_bundle(&self) -> String {
        self.get_or("EVREN_CA_BUNDLE", "").trim().to_owned()
    }

    /// Python varsayılanı `false` idi (kendinden imzalı sertifikalar).
    pub fn ssl_verify_flag(&self) -> bool {
        matches!(
            self.get_or("EVREN_SSL_VERIFY", "false")
                .to_lowercase()
                .as_str(),
            "true" | "1" | "yes"
        )
    }

    /// `get_ssl_verify`: CA demeti yolu varsa o, yoksa doğrulama bayrağı.
    pub fn ssl_verify(&self) -> SslVerify {
        let ca = self.ca_bundle();
        if !ca.is_empty() {
            SslVerify::CaBundle(PathBuf::from(ca))
        } else {
            SslVerify::Flag(self.ssl_verify_flag())
        }
    }

    // ------------------------------------------------------------ kabuk

    /// Git for Windows `bash.exe`; System32 (WSL) ve WindowsApps atlanır.
    pub fn git_bash_path(&self) -> Option<PathBuf> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(o) = self
            .get("EVREN_GIT_BASH")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            candidates.push(PathBuf::from(o));
        }
        if let Some(git) = which("git", self) {
            let git = git.canonicalize().unwrap_or(git);
            for parent in git.ancestors().skip(1).take(3) {
                candidates.push(parent.join("bin").join("bash.exe"));
                candidates.push(parent.join("usr").join("bin").join("bash.exe"));
            }
        }
        for var in [
            "ProgramFiles",
            "ProgramW6432",
            "ProgramFiles(x86)",
            "LOCALAPPDATA",
        ] {
            if let Some(base) = self.get(var).filter(|s| !s.is_empty()) {
                let sub = if var == "LOCALAPPDATA" {
                    Path::new(base).join("Programs").join("Git")
                } else {
                    Path::new(base).join("Git")
                };
                candidates.push(sub.join("bin").join("bash.exe"));
                candidates.push(sub.join("usr").join("bin").join("bash.exe"));
            }
        }
        if let Some(b) = which("bash", self) {
            candidates.push(b);
        }
        candidates.into_iter().find(|c| {
            let low = c.to_string_lossy().to_lowercase().replace('/', "\\");
            !(low.contains("\\windows\\system32\\") || low.contains("windowsapps")) && c.is_file()
        })
    }

    /// `bash` | `cmd` | `powershell`.
    pub fn windows_shell(&self) -> &'static str {
        let choice = self.get_or("EVREN_SHELL", "auto").trim().to_lowercase();
        match choice.as_str() {
            "cmd" => "cmd",
            "powershell" => "powershell",
            "bash" | "auto" | "git-bash" | "gitbash" if self.git_bash_path().is_some() => "bash",
            _ => "cmd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SslVerify {
    CaBundle(PathBuf),
    Flag(bool),
}

fn has_key_env(env: &BTreeMap<String, String>) -> bool {
    env.get("EVREN_API_KEY").is_some_and(|v| !v.is_empty())
        || env.get("EVREN_API_KEYS").is_some_and(|v| !v.is_empty())
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn which(name: &str, cfg: &Config) -> Option<PathBuf> {
    let path = cfg.get("PATH").or_else(|| cfg.get("Path"))?;
    let exts: Vec<String> = if cfg!(windows) {
        cfg.get("PATHEXT")
            .unwrap_or(".EXE;.CMD;.BAT")
            .split(';')
            .map(|e| e.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path.split(sep).filter(|d| !d.is_empty()) {
        for ext in &exts {
            let cand = Path::new(dir).join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// `.env` ayrıştırıcısı: boş/`#`/`=`siz satır atlanır, ilk `=` bölünür,
/// tırnaklar soyulur, ortamda olmayan anahtar yazılır (override=False).
pub fn parse_env_text(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        if !k.is_empty() {
            out.push((k.to_owned(), v.to_owned()));
        }
    }
    out
}

fn load_env_file(path: &Path, env: &mut BTreeMap<String, String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for (k, v) in parse_env_text(&text) {
        env.entry(k).or_insert(v);
    }
}

fn read_json_object(path: &Path) -> Map<String, Value> {
    let Ok(bytes) = std::fs::read(path) else {
        return Map::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// `~/.evren-cli/config.json` → `ApiKeys` listesi ve `BaseUrl`.
/// Dönüş: kullanılan anahtar sayısı (`LEGACY_KEYS_USED`).
fn apply_legacy_config(path: &Path, env: &mut BTreeMap<String, String>) -> usize {
    let data = read_json_object(path);
    if data.is_empty() {
        return 0;
    }
    let keys: Vec<String> = data
        .get("ApiKeys")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut used = 0;
    if !keys.is_empty() {
        let has_key = env.get("EVREN_API_KEY").is_some_and(|v| !v.is_empty());
        let has_keys = env.get("EVREN_API_KEYS").is_some_and(|v| !v.is_empty());
        if !has_key && !has_keys {
            env.insert("EVREN_API_KEYS".into(), keys.join(","));
            env.insert("EVREN_API_KEY".into(), keys[0].clone());
            used = keys.len();
        } else if !has_key {
            let pick = keys
                .iter()
                .find(|k| k.starts_with(API_KEY_PREFIX))
                .unwrap_or(&keys[0]);
            env.insert("EVREN_API_KEY".into(), pick.clone());
            used = keys.len();
        }
    }
    if !env.get("EVREN_BASE_URL").is_some_and(|v| !v.is_empty()) {
        if let Some(u) = data.get("BaseUrl").and_then(Value::as_str).map(str::trim) {
            if !u.is_empty() {
                env.insert("EVREN_BASE_URL".into(), u.to_owned());
            }
        }
    }
    used
}

/// `save_api_keys`: diğer alanları koruyarak `ApiKeys` yazar (indent 2 + `\n`).
pub fn save_api_keys(path: &Path, keys: &[String]) -> Result<(), EvrenError> {
    let mut data = read_json_object(path);
    let mut dedup: Vec<String> = Vec::new();
    for k in keys {
        let k = k.trim();
        if !k.is_empty() && !dedup.iter().any(|x| x == k) {
            dedup.push(k.to_owned());
        }
    }
    data.insert("ApiKeys".into(), Value::from(dedup));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EvrenError::io(parent, e))?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(data))?;
    std::fs::write(path, format!("{text}\n")).map_err(|e| EvrenError::io(path, e))
}

// ---------------------------------------------------------------- dizinler

pub fn get_workspace_dir(custom: Option<&Path>) -> PathBuf {
    let p = match custom {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    p.canonicalize().map(strip_verbatim).unwrap_or(p)
}

/// Windows `\\?\C:\...` ön ekini kaldırır (Python `resolve()` çıktısı gibi).
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

pub fn get_evren_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".evren")
}

pub fn get_backup_dir(workspace_root: &Path) -> PathBuf {
    get_evren_dir(workspace_root).join("backups")
}

/// `<ws>/.evren/tmp` (yapıştırma ve SSH geçici dosyaları).
pub fn get_tmp_dir(workspace_root: &Path) -> PathBuf {
    get_evren_dir(workspace_root).join("tmp")
}

/// `.evren`, `backups`, `logs` dizinlerini açar; `.evren/.gitignore`
/// eksik satırlarla tamamlanır (var olan satırlar silinmez).
pub fn ensure_evren_dirs(workspace_root: &Path) -> Result<PathBuf, EvrenError> {
    let dir = get_evren_dir(workspace_root);
    for d in [dir.clone(), dir.join("backups"), dir.join("logs")] {
        std::fs::create_dir_all(&d).map_err(|e| EvrenError::io(&d, e))?;
    }
    let gi = dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gi).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect();
    let have: Vec<String> = lines.iter().map(|l| l.trim().to_owned()).collect();
    let missing: Vec<&str> = EVREN_GITIGNORE_ENTRIES
        .iter()
        .copied()
        .filter(|e| !have.iter().any(|h| h == e))
        .collect();
    if !missing.is_empty() {
        lines.extend(missing.iter().map(|s| (*s).to_owned()));
        // Python OSError'ı yutar; biz de yutuyoruz.
        let _ = std::fs::write(&gi, format!("{}\n", lines.join("\n")));
    }
    Ok(dir)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Config {
        Config::from_env(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn paste_limits_defaults_overrides_and_invalid_values() {
        let c = cfg(&[]);
        assert_eq!((c.paste_max_lines(), c.paste_max_chars()), (80, 6000));
        let c = cfg(&[
            ("EVREN_PASTE_MAX_LINES", "200"),
            ("EVREN_PASTE_MAX_CHARS", "20000"),
        ]);
        assert_eq!((c.paste_max_lines(), c.paste_max_chars()), (200, 20000));
        for v in ["abc", "0", "-5", ""] {
            let c = cfg(&[("EVREN_PASTE_MAX_LINES", v), ("EVREN_PASTE_MAX_CHARS", v)]);
            assert_eq!(
                (c.paste_max_lines(), c.paste_max_chars()),
                (80, 6000),
                "{v}"
            );
        }
    }

    #[test]
    fn defaults() {
        let c = cfg(&[]);
        assert_eq!(c.base_url(), DEFAULT_BASE_URL);
        assert_eq!(c.default_model(), "mimo-v2.6-pro");
        assert_eq!(c.fallback_model(), "qwen3.8-flash-next");
        assert_eq!(c.fallback_after(), 2);
        assert_eq!(c.default_max_tokens(), 4096);
        assert!((c.default_temperature() - 0.3).abs() < 1e-9);
        assert!(c.auto_approve());
        assert_eq!(c.default_max_steps(), 30);
        assert!((c.compaction_ratio() - 0.6).abs() < 1e-9);
        assert_eq!(c.context_budget(), 64_000);
        assert!(c.smart_compact_enabled());
        assert_eq!(c.summary_model(), "qwen3.8-flash-next");
        assert_eq!(c.ui_theme().unwrap(), "dark");
        assert_eq!(c.reasoning_effort(), Some("low".into()));
        assert_eq!(c.key_min_remaining(), 50_000);
        assert_eq!(c.planner_model(), "deepseek-v4.1-flash");
        assert_eq!(c.executor_model("m"), "m");
        assert!(c.trace_enabled());
        assert_eq!(c.ssl_verify(), SslVerify::Flag(false));
        assert!(c.api_keys().is_empty());
        assert!(!c.check_api_key_configured());
        assert_eq!(c.key_source(), KeySource::NotFound);
        assert_eq!(
            c.require_api_key().unwrap_err().to_string(),
            texts::API_KEY_MISSING
        );
    }

    #[test]
    fn validation_ranges() {
        let c = cfg(&[
            ("EVREN_MAX_STEPS", "0"),
            ("EVREN_COMPACT_RATIO", "5"),
            ("EVREN_CONTEXT_BUDGET", "abc"),
            ("EVREN_UI_THEME", "mavi"),
            ("EVREN_REASONING_EFFORT", "OFF"),
            ("EVREN_TRACE", "hayır"),
            ("EVREN_SMART_COMPACT", "0"),
            ("EVREN_BASE_URL", "http://x/v1///"),
            ("EVREN_CA_BUNDLE", "c:/ca.pem"),
        ]);
        assert_eq!(c.default_max_steps(), 30);
        assert!((c.compaction_ratio() - 0.6).abs() < 1e-9);
        assert_eq!(c.context_budget(), 64_000);
        assert_eq!(
            c.ui_theme().unwrap_err().to_string(),
            texts::UI_THEME_INVALID
        );
        assert_eq!(c.reasoning_effort(), None);
        assert!(!c.trace_enabled());
        assert!(!c.smart_compact_enabled());
        assert_eq!(c.base_url(), "http://x/v1");
        assert_eq!(
            c.ssl_verify(),
            SslVerify::CaBundle(PathBuf::from("c:/ca.pem"))
        );
    }

    #[test]
    fn api_keys_dedup_and_source() {
        let c = cfg(&[
            ("EVREN_API_KEYS", "a1, b2,a1"),
            ("EVREN_API_KEY", "evren_llm_x"),
        ]);
        assert_eq!(c.api_keys(), vec!["a1", "b2", "evren_llm_x"]);
        assert!(c.check_api_key_configured());
        assert_eq!(c.key_source(), KeySource::ProcessEnv);
        let r = c.source_report();
        assert_eq!(r["key_count"], 3);
        assert_eq!(r["key_source"], texts::KEY_SOURCE_PROCESS_ENV);
        assert_eq!(r["has_key"], true);
    }

    #[test]
    fn env_parser() {
        let pairs = parse_env_text("# yorum\n\nA=1\nB = \"iki\"\nC='üç'\nbozuk\nD=a=b\n");
        assert_eq!(
            pairs,
            vec![
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "iki".to_owned()),
                ("C".to_owned(), "üç".to_owned()),
                ("D".to_owned(), "a=b".to_owned()),
            ]
        );
    }

    #[test]
    fn env_file_and_legacy_config_layers() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("ws");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(home.join(".evren-cli")).unwrap();
        std::fs::write(
            cwd.join(".env"),
            "EVREN_DEFAULT_MODEL=dosya\nEVREN_MAX_STEPS=7\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".evren-cli").join("config.json"),
            "\u{feff}{\"ApiKeys\": [\"k1\", \"evren_llm_k2\"], \"BaseUrl\": \"http://legacy/v1/\", \"Other\": 1}",
        )
        .unwrap();
        let mut env = BTreeMap::new();
        env.insert("EVREN_DEFAULT_MODEL".to_owned(), "surec".to_owned());
        let c = Config::from_parts(env, &cwd, &home);
        // Süreç ortamı .env dosyasını ezer (override=False).
        assert_eq!(c.default_model(), "surec");
        assert_eq!(c.default_max_steps(), 7);
        assert_eq!(c.env_file.as_deref(), Some(cwd.join(".env").as_path()));
        // Eski config: her iki anahtar değişkeni de boşsa ikisi birden set edilir.
        assert_eq!(c.api_keys(), vec!["k1", "evren_llm_k2"]);
        assert_eq!(c.api_key(), "k1");
        assert_eq!(c.legacy_keys_used, 2);
        assert_eq!(c.key_source(), KeySource::LegacyConfig);
        assert_eq!(c.base_url(), "http://legacy/v1");
        assert!(c.source_report()["key_source"]
            .as_str()
            .unwrap()
            .contains("(ApiKeys, 2 anahtar)"));

        // Yalnız EVREN_API_KEYS varsa: evren_llm_ öneki tercih edilir.
        let mut env2 = BTreeMap::new();
        env2.insert("EVREN_API_KEYS".to_owned(), "zz".to_owned());
        let c2 = Config::from_parts(env2, &cwd, &home);
        assert_eq!(c2.api_key(), "evren_llm_k2");
        assert_eq!(c2.key_source(), KeySource::ProcessEnv);

        // save_api_keys diğer alanları korur.
        let p = home.join(".evren-cli").join("config.json");
        save_api_keys(&p, &["n1".into(), "n1".into(), "n2".into()]).unwrap();
        let saved = read_json_object(&p);
        assert_eq!(saved["ApiKeys"], serde_json::json!(["n1", "n2"]));
        assert_eq!(saved["Other"], 1);
        assert!(std::fs::read_to_string(&p).unwrap().ends_with('\n'));
    }

    #[test]
    fn evren_dirs_and_gitignore_additive() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        std::fs::create_dir_all(ws.join(".evren")).unwrap();
        std::fs::write(ws.join(".evren/.gitignore"), "benim/\nlogs/\n").unwrap();
        let dir = ensure_evren_dirs(ws).unwrap();
        assert!(dir.join("backups").is_dir());
        assert!(dir.join("logs").is_dir());
        let gi = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        let lines: Vec<&str> = gi.lines().collect();
        assert_eq!(lines[0], "benim/");
        assert_eq!(lines[1], "logs/");
        assert_eq!(lines.iter().filter(|l| **l == "logs/").count(), 1);
        for e in EVREN_GITIGNORE_ENTRIES {
            assert!(lines.contains(&e), "{e}");
        }
        assert!(gi.ends_with('\n'));
    }

    #[test]
    fn shell_choice() {
        assert_eq!(
            cfg(&[("EVREN_SHELL", "powershell")]).windows_shell(),
            "powershell"
        );
        assert_eq!(cfg(&[("EVREN_SHELL", "cmd")]).windows_shell(), "cmd");
        // PATH yok, Git yok → cmd
        assert_eq!(cfg(&[("EVREN_SHELL", "auto")]).windows_shell(), "cmd");
    }
}
