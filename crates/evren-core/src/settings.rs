//! Katmanlı ayarlar, izin kuralları ve kabuk kancaları (`settings.py`).
//!
//! Katmanlar (sonraki kazanır, listeler birleşir):
//! `~/.evren/settings.json` → `<ws>/.evren/settings.json` → `<ws>/.evren/settings.local.json`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::EvrenError;
use crate::secret_vault::{compile_static, SecretVault};
use crate::texts;

pub const HOOK_TIMEOUT: Duration = Duration::from_secs(30);
const HOOK_ARGS_MAX: usize = 8000;

/// Araç çağrısı argümanları (JSON nesnesi).
pub type Args = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    PreTool,
    PostTool,
    Stop,
}

impl HookKind {
    fn key(self) -> &'static str {
        match self {
            HookKind::PreTool => "pre_tool",
            HookKind::PostTool => "post_tool",
            HookKind::Stop => "stop",
        }
    }
}

/// Katmanları birleştirmiş ayar görünümü.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub data: Map<String, Value>,
    pub workspace: Option<PathBuf>,
    /// CLI `--allowed-tools` (araç adları); her zaman izinli.
    pub extra_allow: Vec<String>,
}

pub fn home_settings_file() -> PathBuf {
    crate::config::home_dir()
        .join(".evren")
        .join("settings.json")
}

fn parse_file(path: &Path) -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Map::new();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Sözlükler özyinelemeli birleşir, listeler tekrarsız eklenir, diğerleri ezilir.
pub fn merge(a: &Map<String, Value>, b: &Map<String, Value>) -> Map<String, Value> {
    let mut out = a.clone();
    for (k, v) in b {
        match (out.get(k), v) {
            (Some(Value::Object(x)), Value::Object(y)) => {
                let merged = merge(x, y);
                out.insert(k.clone(), Value::Object(merged));
            }
            (Some(Value::Array(x)), Value::Array(y)) => {
                let mut list = x.clone();
                for item in y {
                    if !list.contains(item) {
                        list.push(item.clone());
                    }
                }
                out.insert(k.clone(), Value::Array(list));
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

impl Settings {
    /// Üç katmanı okur ve birleştirir.
    pub fn load(workspace: Option<&Path>) -> Self {
        Self::load_with_home(workspace, &home_settings_file())
    }

    pub fn load_with_home(workspace: Option<&Path>, home_file: &Path) -> Self {
        let mut data = parse_file(home_file);
        if let Some(ws) = workspace {
            data = merge(&data, &parse_file(&ws.join(".evren").join("settings.json")));
            data = merge(
                &data,
                &parse_file(&ws.join(".evren").join("settings.local.json")),
            );
        }
        Settings {
            data,
            workspace: workspace.map(Path::to_path_buf),
            extra_allow: Vec::new(),
        }
    }

    pub fn from_value(data: Map<String, Value>) -> Self {
        Settings {
            data,
            workspace: None,
            extra_allow: Vec::new(),
        }
    }

    fn rule_list(&self, which: &str) -> Vec<String> {
        self.data
            .get("permissions")
            .and_then(Value::as_object)
            .and_then(|p| p.get(which))
            .and_then(Value::as_array)
            .map(|a| a.iter().map(value_to_string).collect())
            .unwrap_or_default()
    }

    /// `deny` kazanır; `None` → kullanıcıya sor.
    pub fn check_permission(&self, tool: &str, args: &Args) -> Option<Permission> {
        if self.extra_allow.iter().any(|t| t == tool) {
            return Some(Permission::Allow);
        }
        if self
            .rule_list("deny")
            .iter()
            .any(|r| rule_matches(r, tool, args))
        {
            return Some(Permission::Deny);
        }
        if self
            .rule_list("allow")
            .iter()
            .any(|r| rule_matches(r, tool, args))
        {
            return Some(Permission::Allow);
        }
        None
    }

    /// Eşleşen kanca komutları (`matcher`: `*`, fnmatch ya da tam regex).
    pub fn hooks(&self, kind: HookKind, tool: &str) -> Vec<String> {
        let mut out = Vec::new();
        let list = self
            .data
            .get("hooks")
            .and_then(Value::as_object)
            .and_then(|h| h.get(kind.key()))
            .and_then(Value::as_array);
        for h in list.into_iter().flatten() {
            let Some(obj) = h.as_object() else { continue };
            let Some(cmd) = obj.get("command").filter(|c| !c.is_null()) else {
                continue;
            };
            let cmd = value_to_string(cmd);
            if cmd.is_empty() {
                continue;
            }
            let matcher = obj
                .get("matcher")
                .filter(|m| !m.is_null())
                .map(value_to_string)
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "*".to_owned());
            let hit = matcher == "*"
                || fnmatchcase(tool, &matcher)
                || Regex::new(&format!("^(?:{matcher})$")).is_ok_and(|re| re.is_match(tool));
            if hit {
                out.push(cmd);
            }
        }
        out
    }

    /// `pre_tool` kancası sıfır dışı çıkarsa engel gerekçesi döner.
    pub fn run_pre_tool(&self, tool: &str, args: &Args, vault: &SecretVault) -> Option<String> {
        let env = hook_env(tool, args, vault);
        for cmd in self.hooks(HookKind::PreTool, tool) {
            let (code, out) = run_hook(&cmd, &env, self.workspace.as_deref());
            if code != 0 {
                return Some(if out.is_empty() {
                    texts::pre_tool_hook_blocked(code)
                } else {
                    out
                });
            }
        }
        None
    }

    pub fn run_post_tool(&self, tool: &str, args: &Args, vault: &SecretVault) {
        let env = hook_env(tool, args, vault);
        for cmd in self.hooks(HookKind::PostTool, tool) {
            run_hook(&cmd, &env, self.workspace.as_deref());
        }
    }

    pub fn run_stop(&self) {
        let env = [
            ("EVREN_TOOL_NAME", String::new()),
            ("EVREN_TOOL_ARGS", "{}".to_owned()),
        ];
        for cmd in self.hooks(HookKind::Stop, "") {
            run_hook(&cmd, &env, self.workspace.as_deref());
        }
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- kurallar

fn rule_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile_static(r"(?s)^\s*([\w\-]+)\s*(?:\((.*)\))?\s*$"))
}

fn arg_str(args: &Args, key: &str) -> Option<String> {
    args.get(key).filter(|v| truthy(v)).map(value_to_string)
}

/// Python `bool()` yaklaşımı: boş dize, 0, false, null, boş liste/nesne → yanlış.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Kuralın parantez içiyle karşılaştırılan özne.
pub fn subject(tool: &str, args: &Args) -> String {
    if tool == "ssh_run" {
        let cmd = arg_str(args, "command")
            .unwrap_or_else(|| format!("preset:{}", arg_str(args, "preset").unwrap_or_default()));
        return format!("{}:{cmd}", arg_str(args, "alias").unwrap_or_default());
    }
    if tool == "ssh_write_file" || tool == "ssh_read_file" {
        return format!(
            "{}:{}",
            arg_str(args, "alias").unwrap_or_default(),
            arg_str(args, "remote_path").unwrap_or_default()
        );
    }
    for key in ["command", "path", "url", "name", "query", "q"] {
        if let Some(v) = arg_str(args, key) {
            return v;
        }
    }
    String::new()
}

pub fn rule_matches(rule: &str, tool: &str, args: &Args) -> bool {
    let Some(m) = rule_re().captures(rule) else {
        return false;
    };
    if m.get(1).map(|g| g.as_str()) != Some(tool) {
        return false;
    }
    let Some(pat) = m.get(2).map(|g| g.as_str()) else {
        return true;
    };
    let pat = pat.trim();
    if pat.is_empty() || pat == "*" {
        return true;
    }
    let pat = if tool == "ssh_run" {
        static COMMA: OnceLock<Regex> = OnceLock::new();
        COMMA
            .get_or_init(|| compile_static(r"\s*,\s*"))
            .replacen(pat, 1, ":")
            .into_owned()
    } else {
        pat.to_owned()
    };
    let subj = subject(tool, args).replace('\\', "/");
    fnmatchcase(&subj, &pat.replace('\\', "/"))
}

/// Onay isteminde "her zaman izin ver" seçildiğinde kaydedilecek kural.
pub fn rule_for(tool: &str, args: &Args) -> String {
    let mut subj = subject(tool, args);
    if subj.is_empty() {
        return tool.to_owned();
    }
    if tool == "run_command" {
        let words: Vec<&str> = subj.split_whitespace().collect();
        let mut s = if words.len() > 1 {
            words[..2].join(" ")
        } else {
            subj.clone()
        };
        s.push('*');
        if words.len() > 1
            && !matches!(
                words[0],
                "git" | "npm" | "kubectl" | "docker" | "pip" | "python"
            )
        {
            s = format!("{} *", words[0]);
        }
        subj = s;
    } else if tool == "ssh_run" {
        let (alias, cmd) = subj.split_once(':').unwrap_or((&subj, ""));
        let w: Vec<&str> = cmd.split_whitespace().collect();
        let head = w.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        subj = format!("{alias}:{head}*");
    }
    format!("{tool}({subj})")
}

/// `<ws>/.evren/settings.local.json` içine `permissions.allow` kuralı ekler.
pub fn add_allow_rule(workspace: &Path, rule: &str) -> Result<PathBuf, EvrenError> {
    let path = workspace.join(".evren").join("settings.local.json");
    let mut data = parse_file(&path);
    let perms = data
        .entry("permissions")
        .or_insert_with(|| Value::Object(Map::new()));
    if !perms.is_object() {
        *perms = Value::Object(Map::new());
    }
    if let Some(p) = perms.as_object_mut() {
        let allow = p.entry("allow").or_insert_with(|| Value::Array(Vec::new()));
        if !allow.is_array() {
            *allow = Value::Array(Vec::new());
        }
        if let Some(list) = allow.as_array_mut() {
            let r = Value::from(rule);
            if !list.contains(&r) {
                list.push(r);
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EvrenError::io(parent, e))?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(data))?;
    std::fs::write(&path, text).map_err(|e| EvrenError::io(&path, e))?;
    Ok(path)
}

/// Python `fnmatch.fnmatchcase`: `*`, `?`, `[seq]`, `[!seq]`; büyük/küçük harf duyarlı.
pub fn fnmatchcase(name: &str, pat: &str) -> bool {
    let re = fnmatch_to_regex(pat);
    Regex::new(&re).is_ok_and(|r| r.is_match(name))
}

fn fnmatch_to_regex(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::from("(?s)^");
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => {
                let mut j = i;
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
                    out.push_str("\\[");
                } else {
                    let mut body: String = chars[i..j].iter().collect();
                    i = j + 1;
                    body = body.replace('\\', "\\\\");
                    if let Some(rest) = body.strip_prefix('!') {
                        body = format!("^{rest}");
                    } else if let Some(rest) = body.strip_prefix('^') {
                        body = format!("\\^{rest}");
                    }
                    out.push('[');
                    out.push_str(&body);
                    out.push(']');
                }
            }
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    out
}

// ---------------------------------------------------------------- kancalar

fn hook_env(tool: &str, args: &Args, vault: &SecretVault) -> [(&'static str, String); 2] {
    let json = serde_json::to_string(&Value::Object(args.clone())).unwrap_or_else(|_| "{}".into());
    let masked: String = vault.mask(&json).chars().take(HOOK_ARGS_MAX).collect();
    [
        ("EVREN_TOOL_NAME", tool.to_owned()),
        ("EVREN_TOOL_ARGS", masked),
    ]
}

/// Komutu kabukta çalıştırır (30 sn); `(çıkış kodu, stdout ya da stderr)`.
/// Başlatma hatasında Python gibi `(0, "hook hatası: ...")` döner.
pub fn run_hook(cmd: &str, env: &[(&str, String)], cwd: Option<&Path>) -> (i32, String) {
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    command
        .current_dir(cwd.unwrap_or_else(|| Path::new(".")))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return (0, texts::hook_error(&e)),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let reader = |mut s: Option<std::process::ChildStdout>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = s.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_t = reader(stdout);
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() < HOOK_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Err(e) => return (0, texts::hook_error(&e)),
        }
    };
    let Some(status) = status else {
        return (
            0,
            texts::hook_error(&format!("zaman aşımı ({} sn)", HOOK_TIMEOUT.as_secs())),
        );
    };
    let out = String::from_utf8_lossy(&out_t.join().unwrap_or_default())
        .trim()
        .to_owned();
    let err = String::from_utf8_lossy(&err_t.join().unwrap_or_default())
        .trim()
        .to_owned();
    let code = status.code().unwrap_or(1);
    (code, if out.is_empty() { err } else { out })
}

/// Ortam haritası yardımcıları (alt süreçlere kasa değişkenlerini eklemek için).
pub fn env_with(extra: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    for (k, v) in extra {
        env.retain(|(ek, _)| ek != k);
        env.push((k.clone(), v.clone()));
    }
    env
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(v: Value) -> Args {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn merge_semantics() {
        let a = args(json!({"permissions": {"allow": ["x", "y"]}, "k": 1, "o": {"a": 1}}));
        let b = args(
            json!({"permissions": {"allow": ["y", "z"], "deny": ["d"]}, "k": 2, "o": {"b": 2}}),
        );
        let m = merge(&a, &b);
        assert_eq!(m["permissions"]["allow"], json!(["x", "y", "z"]));
        assert_eq!(m["permissions"]["deny"], json!(["d"]));
        assert_eq!(m["k"], 2);
        assert_eq!(m["o"], json!({"a": 1, "b": 2}));
    }

    #[test]
    fn fnmatch_basics() {
        assert!(fnmatchcase("git status", "git *"));
        assert!(!fnmatchcase("Git status", "git *"));
        assert!(fnmatchcase("a.txt", "?.txt"));
        assert!(fnmatchcase("src/x.rs", "src/*.rs"));
        assert!(fnmatchcase("b", "[abc]"));
        assert!(!fnmatchcase("d", "[abc]"));
        assert!(fnmatchcase("d", "[!abc]"));
        assert!(fnmatchcase("x[", "x["));
        assert!(fnmatchcase("a\nb", "a*b"));
    }

    #[test]
    fn permission_rules() {
        let s = Settings::from_value(args(json!({
            "permissions": {
                "allow": ["read_file", "run_command(git *)", "ssh_run(prod, systemctl status*)"],
                "deny": ["run_command(git push*)", "write_file(C:\\secret\\*)"]
            }
        })));
        assert_eq!(
            s.check_permission("read_file", &args(json!({"path": "x"}))),
            Some(Permission::Allow)
        );
        assert_eq!(
            s.check_permission("run_command", &args(json!({"command": "git status"}))),
            Some(Permission::Allow)
        );
        assert_eq!(
            s.check_permission("run_command", &args(json!({"command": "git push origin"}))),
            Some(Permission::Deny)
        );
        assert_eq!(
            s.check_permission("run_command", &args(json!({"command": "rm -rf"}))),
            None
        );
        assert_eq!(
            s.check_permission("write_file", &args(json!({"path": "C:/secret/a.txt"}))),
            Some(Permission::Deny)
        );
        assert_eq!(
            s.check_permission(
                "ssh_run",
                &args(json!({"alias": "prod", "command": "systemctl status nginx"}))
            ),
            Some(Permission::Allow)
        );
        let mut s2 = s.clone();
        s2.extra_allow.push("run_command".into());
        assert_eq!(
            s2.check_permission("run_command", &args(json!({"command": "git push"}))),
            Some(Permission::Allow)
        );
    }

    #[test]
    fn rule_for_shapes() {
        assert_eq!(rule_for("read_file", &args(json!({}))), "read_file");
        assert_eq!(
            rule_for("read_file", &args(json!({"path": "a/b"}))),
            "read_file(a/b)"
        );
        assert_eq!(
            rule_for("run_command", &args(json!({"command": "git status -s"}))),
            "run_command(git status*)"
        );
        assert_eq!(
            rule_for(
                "run_command",
                &args(json!({"command": "cargo build --release"}))
            ),
            "run_command(cargo *)"
        );
        assert_eq!(
            rule_for("run_command", &args(json!({"command": "ls"}))),
            "run_command(ls*)"
        );
        assert_eq!(
            rule_for(
                "ssh_run",
                &args(json!({"alias": "prod", "command": "systemctl status nginx"}))
            ),
            "ssh_run(prod:systemctl status*)"
        );
        assert_eq!(
            rule_for("ssh_run", &args(json!({"alias": "prod", "preset": "disk"}))),
            "ssh_run(prod:preset:disk*)"
        );
    }

    #[test]
    fn add_allow_rule_writes_local_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = add_allow_rule(tmp.path(), "run_command(git *)").unwrap();
        add_allow_rule(tmp.path(), "run_command(git *)").unwrap();
        add_allow_rule(tmp.path(), "read_file").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["permissions"]["allow"],
            json!(["run_command(git *)", "read_file"])
        );
        assert!(text.contains("\n  \"permissions\""), "indent=2 bekleniyor");
        let s = Settings::load_with_home(Some(tmp.path()), &tmp.path().join("yok.json"));
        assert_eq!(
            s.check_permission("read_file", &args(json!({}))),
            Some(Permission::Allow)
        );
    }

    #[test]
    fn hooks_match_and_run() {
        let s = Settings::from_value(args(json!({
            "hooks": {
                "pre_tool": [
                    {"command": "exit 3", "matcher": "run_*"},
                    {"command": "echo hi"},
                    {"matcher": "*"}
                ],
                "post_tool": [{"command": "echo post", "matcher": "read_file|write_file"}]
            }
        })));
        assert_eq!(
            s.hooks(HookKind::PreTool, "run_command"),
            vec!["exit 3", "echo hi"]
        );
        assert_eq!(s.hooks(HookKind::PreTool, "read_file"), vec!["echo hi"]);
        assert_eq!(s.hooks(HookKind::PostTool, "write_file"), vec!["echo post"]);
        assert!(s.hooks(HookKind::PostTool, "run_command").is_empty());
        let vault = SecretVault::new();
        let reason = s.run_pre_tool("run_command", &args(json!({"command": "x"})), &vault);
        assert_eq!(reason.as_deref(), Some("pre_tool hook engelledi (çıkış 3)"));
        assert_eq!(s.run_pre_tool("read_file", &args(json!({})), &vault), None);
        let (code, out) = run_hook(
            "echo %EVREN_TOOL_NAME%$EVREN_TOOL_NAME",
            &[("EVREN_TOOL_NAME", "t".into())],
            None,
        );
        assert_eq!(code, 0);
        assert!(out.contains('t'));
    }
}
