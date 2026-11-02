//! Rol profilleri (`--role <ad>` / `--profile <dosya>`, Python `roles.py`).
//!
//! Bir profil ajanı tek bir işe daraltır (analyst, developer, tester,
//! shipper): ek sistem talimatı, çağırabileceği araçlar, yazabileceği yollar,
//! kabuk komutu ve ağ sınırları, adım / token / tekrar bütçeleri. Sınırlar
//! yalnız istemde değil araç kapısında uygulanır.
//!
//! Profil dosyası JSON (her alan isteğe bağlı); TOML dosyaları bu sürümde
//! desteklenmez. `--role X` araması: `<çalışma alanı>/.evren/roles/X.json`,
//! `~/.evren/roles/X.json`, sonra yerleşikler.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use evren_core::tools::fs_tools::fnmatch;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::guards::command_may_modify_files;

const WRITE_TOOLS: [&str; 5] = [
    "edit_file",
    "write_file",
    "apply_patch",
    "ssh_write_file",
    "clone_tree",
];

const READ: [&str; 5] = [
    "view_file",
    "list_directory",
    "find_files",
    "search_code",
    "todo_write",
];

macro_rules! re {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            #[allow(clippy::expect_used)]
            RE.get_or_init(|| Regex::new($pat).expect("geçerli düzenli ifade"))
        }
    };
}

re!(
    patch_file_re,
    r"(?m)^\*\*\* (?:Add|Update|Delete) File:\s*(.+?)\s*$|^\*\*\* Move to:\s*(.+?)\s*$"
);
re!(url_host_re, r#"(?i)\bhttps?://([^/\s:'"`]+)"#);
re!(strip_cd_re, r"^\s*(?:cd\s+\S+\s*(?:&&|;)\s*)+");

pub const BUILTIN_NAMES: [&str; 4] = ["analyst", "developer", "tester", "shipper"];

fn with_read(extra: &[&str]) -> Value {
    let mut v: Vec<&str> = READ.to_vec();
    v.extend_from_slice(extra);
    json!(v)
}

fn builtin(name: &str) -> Option<Value> {
    let v = match name {
        "analyst" => json!({
            "prompt": "Rolün ANALİST. Kod yazmazsın. Görevi oku, ilgili dosyaları bul ve yalnız spec.md yaz: amaç, etkilenen dosyalar, ölçülebilir kabul ölçütleri (her biri bir komut ya da test adıyla), kapsam dışı, riskler. Belirsizlik varsa tek soru yaz.",
            "tools": with_read(&["web_fetch", "write_file", "edit_file"]),
            "write_paths": ["**/spec.md", "**/plan.json"],
            "report": "spec.md",
            "max_steps": 20,
            "repeat_block": 3,
            "repeat_stuck": 5,
        }),
        "developer" => json!({
            "prompt": "Rolün GELİŞTİRİCİ. spec.md'deki kabul ölçütlerini karşılayan değişikliği yap ve dev-report.md yaz. Kabul ölçütündeki testleri en fazla bir kez koş; test döngüsü testçinin işi.",
            "tools": with_read(&["edit_file", "write_file", "apply_patch", "run_command"]),
            "max_test_runs": 4,
            "report": "dev-report.md",
            "max_steps": 40,
            "repeat_block": 3,
            "repeat_stuck": 5,
        }),
        "tester" => json!({
            "prompt": "Rolün TESTÇİ. Kaynak koda yazamazsın; yalnız tests/ altına ve test-report.md'ye yazarsın. Her kabul ölçütü için komutu koş, GEÇTİ/KALDI + komut + çıktı kesiti (KANIT) yaz. Canlı host'lara gitme.",
            "tools": with_read(&["write_file", "edit_file", "run_command"]),
            "write_paths": ["tests/**", "test/**", "**/tests/**", "**/test-report.md", "**/*.test.*", "**/*_test.*"],
            "shell_writes": false,
            "allowed_hosts": ["localhost", "127.0.0.1"],
            "report": "test-report.md",
            "max_steps": 30,
            "repeat_block": 3,
            "repeat_stuck": 5,
        }),
        "shipper" => json!({
            "prompt": "Rolün GÖNDERİCİ. Sürümü yükselt, CHANGELOG yaz, commit at, ship-report.md yaz. Push etme; push ve canlıya dokunan adımlar insan onayı ister. Metinlerde AI imzası yok.",
            "tools": with_read(&["edit_file", "write_file", "run_command"]),
            "write_paths": ["CHANGELOG.md", "**/CHANGELOG.md", "pyproject.toml", "package.json", "**/version.*", "**/ship-report.md", "VERSION"],
            "commands": [r"^git (status|diff|log|add|commit|tag|branch|rev-parse|show|merge|switch|checkout)\b"],
            "allowed_hosts": [],
            "report": "ship-report.md",
            "max_steps": 20,
            "repeat_block": 3,
            "repeat_stuck": 5,
        }),
        _ => return None,
    };
    Some(v)
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoleProfile {
    pub name: String,
    pub prompt: String,
    pub tools: Option<Vec<String>>,
    pub write_paths: Option<Vec<String>>,
    pub commands: Option<Vec<String>>,
    pub shell_writes: bool,
    pub allowed_hosts: Option<Vec<String>>,
    pub max_steps: Option<u32>,
    pub token_budget: Option<u64>,
    pub repeat_block: u32,
    pub repeat_stuck: u32,
    pub max_test_runs: Option<u32>,
    pub report: String,
}

fn str_list(data: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    match data.get(key)? {
        Value::Array(a) => Some(
            a.iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect(),
        ),
        Value::Null => None,
        Value::String(s) => Some(vec![s.clone()]),
        _ => None,
    }
}

fn uint(data: &Map<String, Value>, key: &str) -> Option<u64> {
    match data.get(key)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

impl RoleProfile {
    pub fn from_value(data: &Value, name: &str) -> RoleProfile {
        let empty = Map::new();
        let data = data.as_object().unwrap_or(&empty);
        let text = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let given = text("name");
        let name = if !given.is_empty() {
            given
        } else if !name.is_empty() {
            name.to_string()
        } else {
            "custom".to_string()
        };
        let repeat_block = uint(data, "repeat_block").unwrap_or(3) as u32;
        let repeat_stuck = (uint(data, "repeat_stuck").unwrap_or(5) as u32).max(repeat_block);
        RoleProfile {
            name,
            prompt: text("prompt"),
            tools: str_list(data, "tools"),
            write_paths: str_list(data, "write_paths"),
            commands: str_list(data, "commands"),
            shell_writes: data
                .get("shell_writes")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            allowed_hosts: str_list(data, "allowed_hosts"),
            max_steps: uint(data, "max_steps").map(|v| v as u32),
            token_budget: uint(data, "token_budget"),
            repeat_block,
            repeat_stuck,
            max_test_runs: uint(data, "max_test_runs").map(|v| v as u32),
            report: text("report"),
        }
    }

    /// Rolün çağıramayacağı araç şemalarını düşürür (kapı zaten reddeder).
    pub fn advertised(&self, tools: Vec<Value>) -> Vec<Value> {
        let Some(allowed) = &self.tools else {
            return tools;
        };
        tools
            .into_iter()
            .filter(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(|n| allowed.iter().any(|a| a == n))
            })
            .collect()
    }

    /// Ret gerekçesi ya da rol çağrıya izin veriyorsa boş dize. `rel(raw)`
    /// araç yolunu `write_paths` ile karşılaştırılacak göreli biçime çevirir.
    pub fn check(&self, name: &str, args: &Value, rel: &dyn Fn(&str) -> String) -> String {
        if let Some(tools) = &self.tools {
            if !tools.iter().any(|t| t == name) {
                return format!(
                    "'{}' rolü '{name}' aracını kullanamaz (izinli: {}).",
                    self.name,
                    tools.join(", ")
                );
            }
        }
        if WRITE_TOOLS.contains(&name) {
            if let Some(paths) = &self.write_paths {
                for raw in write_targets(name, args) {
                    if !matches(&rel(&raw), paths) {
                        let allowed = if paths.is_empty() {
                            "hiçbir yol".to_string()
                        } else {
                            paths.join(", ")
                        };
                        return format!(
                            "'{}' rolü '{raw}' yoluna yazamaz (izinli: {allowed}).",
                            self.name
                        );
                    }
                }
            }
        }
        let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        if name == "run_command" || name == "ssh_run" {
            let command = arg("command");
            if let Some(cmds) = &self.commands {
                let stripped = strip_cd_re().replace(&command, "").into_owned();
                let ok = cmds
                    .iter()
                    .any(|p| Regex::new(p).is_ok_and(|r| r.is_match(&stripped)));
                if !ok {
                    return format!(
                        "'{}' rolü bu komutu çalıştıramaz; izinli kalıplar: {}.",
                        self.name,
                        cmds.join(", ")
                    );
                }
            }
            if !self.shell_writes && command_may_modify_files(&command) {
                return format!(
                    "'{}' rolü dosya değiştiren kabuk komutu çalıştıramaz; izinli dosyalara edit_file/write_file ile yaz.",
                    self.name
                );
            }
            let host = self.foreign_host(&command);
            if !host.is_empty() {
                return format!(
                    "'{}' rolü '{host}' adresine ağ erişimi yapamaz (izinli: {}).",
                    self.name,
                    self.hosts_text()
                );
            }
        }
        if name == "web_fetch" {
            let host = self.foreign_host(&arg("url"));
            if !host.is_empty() {
                return format!(
                    "'{}' rolü '{host}' adresine ağ erişimi yapamaz (izinli: {}).",
                    self.name,
                    self.hosts_text()
                );
            }
        }
        String::new()
    }

    fn foreign_host(&self, text: &str) -> String {
        let Some(hosts) = &self.allowed_hosts else {
            return String::new();
        };
        let allowed: Vec<String> = hosts.iter().map(|h| h.to_lowercase()).collect();
        for cap in url_host_re().captures_iter(text) {
            let host = cap.get(1).map_or("", |m| m.as_str()).to_lowercase();
            if allowed
                .iter()
                .any(|a| host == *a || host.ends_with(&format!(".{a}")))
            {
                continue;
            }
            return host;
        }
        String::new()
    }

    fn hosts_text(&self) -> String {
        let joined = self.allowed_hosts.clone().unwrap_or_default().join(", ");
        if joined.is_empty() {
            "hiçbiri".to_string()
        } else {
            joined
        }
    }

    pub fn prompt_block(&self) -> String {
        let mut lines = vec![format!("[ROL: {}]", self.name)];
        if !self.prompt.trim().is_empty() {
            lines.push(self.prompt.trim().to_string());
        }
        if let Some(paths) = &self.write_paths {
            let joined = if paths.is_empty() {
                "yok (salt okunur)".to_string()
            } else {
                paths.join(", ")
            };
            lines.push(format!("Yazabileceğin yollar: {joined}."));
        }
        if self.allowed_hosts.is_some() {
            lines.push(format!("Ağ erişimi yalnız: {}.", self.hosts_text()));
        }
        if let Some(n) = self.max_test_runs {
            lines.push(format!(
                "Test/derleme komutunu bu görevde en çok {n} kez koşabilirsin; sonrası reddedilir. Düzenlemeleri toplayıp tek seferde doğrula."
            ));
        }
        if !self.report.is_empty() {
            lines.push(format!(
                "Görev sonunda {} yazılmış olmalı; adım biterken önce onu yaz.",
                self.report
            ));
        }
        lines.push(format!(
            "Aynı çağrıyı aynı argümanla {}. kez yaparsan reddedilir; o noktada hipotezini değiştir ya da engeli raporla.",
            self.repeat_block
        ));
        lines.join("\n")
    }
}

fn write_targets(name: &str, args: &Value) -> Vec<String> {
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match name {
        "apply_patch" => patch_file_re()
            .captures_iter(&arg("patch"))
            .map(|c| {
                c.get(1)
                    .or_else(|| c.get(2))
                    .map_or(String::new(), |m| m.as_str().to_string())
            })
            .collect(),
        "ssh_write_file" => vec![arg("remote_path")],
        "clone_tree" => {
            let v = [arg("dest"), arg("target"), arg("path")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or_default();
            vec![v]
        }
        _ => vec![arg("path")],
    }
}

fn matches(rel: &str, patterns: &[String]) -> bool {
    let rel = rel.replace('\\', "/");
    patterns.iter().any(|pat| {
        let pat = pat.replace('\\', "/");
        fnmatch(&rel, &pat) || pat.strip_prefix("**/").is_some_and(|p| fnmatch(&rel, p))
    })
}

fn read_file(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
    {
        return Err(format!(
            "{}: TOML rol dosyaları bu sürümde desteklenmiyor; JSON kullanın.",
            path.display()
        ));
    }
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `spec` profil dosyası yolu ya da rol adıdır; bilinmiyorsa hata metni döner.
pub fn load_profile(spec: &str, workspace: Option<&Path>) -> Result<RoleProfile, String> {
    let path = Path::new(spec);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if (ext == "toml" || ext == "json") && path.is_file() {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok(RoleProfile::from_value(&read_file(path)?, &stem));
    }
    let mut dirs = Vec::new();
    if let Some(ws) = workspace {
        dirs.push(ws.join(".evren").join("roles"));
    }
    if let Some(home) = home_dir() {
        dirs.push(home.join(".evren").join("roles"));
    }
    for d in dirs {
        for ext in ["toml", "json"] {
            let cand = d.join(format!("{spec}.{ext}"));
            if cand.is_file() {
                return Ok(RoleProfile::from_value(&read_file(&cand)?, spec));
            }
        }
    }
    if let Some(v) = builtin(spec) {
        return Ok(RoleProfile::from_value(&v, spec));
    }
    Err(format!(
        "Rol/profil bulunamadı: {spec} (yerleşik: {}; ya da .toml/.json dosya yolu)",
        BUILTIN_NAMES.join(", ")
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rel(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn builtins_load() {
        for n in BUILTIN_NAMES {
            let p = load_profile(n, None).unwrap();
            assert_eq!(p.name, n);
            assert_eq!(p.repeat_block, 3);
            assert_eq!(p.repeat_stuck, 5);
        }
        assert!(load_profile("yok", None)
            .unwrap_err()
            .starts_with("Rol/profil bulunamadı: yok"));
    }

    #[test]
    fn tester_gate() {
        let p = load_profile("tester", None).unwrap();
        assert!(p
            .check("apply_patch", &json!({}), &rel)
            .contains("aracını kullanamaz"));
        assert_eq!(
            p.check("write_file", &json!({"path": "tests/test_a.py"}), &rel),
            ""
        );
        assert_eq!(
            p.check("write_file", &json!({"path": "test-report.md"}), &rel),
            ""
        );
        assert!(p
            .check("write_file", &json!({"path": "src/a.py"}), &rel)
            .contains("yoluna yazamaz"));
        assert!(p
            .check("run_command", &json!({"command": "sed -i s/a/b/ x.py"}), &rel)
            .contains("dosya değiştiren"));
        assert!(p
            .check(
                "run_command",
                &json!({"command": "curl https://api.example.com/x"}),
                &rel
            )
            .contains("'api.example.com' adresine"));
        assert_eq!(
            p.check(
                "run_command",
                &json!({"command": "curl http://localhost:8000/x"}),
                &rel
            ),
            ""
        );
    }

    #[test]
    fn shipper_commands() {
        let p = load_profile("shipper", None).unwrap();
        assert_eq!(
            p.check("run_command", &json!({"command": "cd x && git status"}), &rel),
            ""
        );
        assert!(p
            .check("run_command", &json!({"command": "rm -rf x"}), &rel)
            .contains("izinli kalıplar"));
    }

    #[test]
    fn prompt_and_advertised() {
        let p = load_profile("analyst", None).unwrap();
        let block = p.prompt_block();
        assert!(block.starts_with("[ROL: analyst]\nRolün ANALİST."));
        assert!(block.contains("Yazabileceğin yollar: **/spec.md, **/plan.json."));
        let tools = vec![
            json!({"function": {"name": "view_file"}}),
            json!({"function": {"name": "run_command"}}),
        ];
        assert_eq!(p.advertised(tools).len(), 1);
    }
}
