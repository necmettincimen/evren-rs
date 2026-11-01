//! SSH host kayıt defteri (`~/.evren/ssh_hosts.json` ve `<ws>/.evren/ssh_hosts.json`),
//! Python `ssh_hosts.py` paritesi. Dosya biçimi `{alias: {host, user, port,
//! identity_file, description, allow_write}}`; yazım Python `json.dumps(indent=2,
//! ensure_ascii=False)` ile bayt bayt aynıdır (sonda yeni satır yok).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{get_evren_dir, get_workspace_dir, home_dir};
use crate::error::EvrenError;

/// Kayıtlı bir SSH hedefi.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshHost {
    #[serde(skip)]
    pub alias: String,
    pub host: String,
    #[serde(default)]
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub identity_file: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub allow_write: bool,
}

fn default_port() -> u16 {
    22
}

impl SshHost {
    pub fn new(alias: &str, host: &str) -> Self {
        Self {
            alias: alias.to_string(),
            host: host.to_string(),
            user: String::new(),
            port: 22,
            identity_file: String::new(),
            description: String::new(),
            allow_write: false,
        }
    }

    /// `user@host` ya da yalnız `host`.
    pub fn target(&self) -> String {
        if self.user.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.user, self.host)
        }
    }

    /// Python `to_dict` (alias hariç, alan sırası sabit).
    pub fn to_dict(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("host".into(), Value::String(self.host.clone()));
        m.insert("user".into(), Value::String(self.user.clone()));
        m.insert("port".into(), Value::from(self.port));
        m.insert(
            "identity_file".into(),
            Value::String(self.identity_file.clone()),
        );
        m.insert(
            "description".into(),
            Value::String(self.description.clone()),
        );
        m.insert("allow_write".into(), Value::Bool(self.allow_write));
        m
    }

    /// Python `from_dict`: alanlar `str(...).strip()`, `port = int(port or 22)`,
    /// `allow_write = bool(...)`.
    pub fn from_dict(alias: &str, data: &Map<String, Value>) -> Self {
        let s = |k: &str| -> String {
            match data.get(k) {
                None => String::new(),
                Some(Value::String(v)) => v.trim().to_string(),
                Some(other) => py_str(other).trim().to_string(),
            }
        };
        let port = match data.get("port") {
            Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Some(Value::String(v)) => v.trim().parse::<i64>().ok(),
            Some(Value::Bool(true)) => Some(1),
            _ => None,
        }
        .filter(|p| *p != 0)
        .unwrap_or(22);
        let port = u16::try_from(port).unwrap_or(22);
        Self {
            alias: alias.to_string(),
            host: s("host"),
            user: s("user"),
            port,
            identity_file: s("identity_file"),
            description: s("description"),
            allow_write: data.get("allow_write").map(py_truthy).unwrap_or(false),
        }
    }
}

/// Python `str(value)` yaklaşık karşılığı (`None` → "None", bool → True/False).
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Alias → host; ekleme sırası korunur (Python `dict`).
pub type HostMap = Vec<SshHost>;

/// `HostMap` üzerinde sözlük benzeri yardımcılar.
pub trait HostMapExt {
    fn get_alias(&self, alias: &str) -> Option<&SshHost>;
    fn upsert(&mut self, host: SshHost);
    fn remove_alias(&mut self, alias: &str) -> bool;
    fn aliases(&self) -> Vec<String>;
}

impl HostMapExt for HostMap {
    fn get_alias(&self, alias: &str) -> Option<&SshHost> {
        self.iter().find(|h| h.alias == alias)
    }

    fn upsert(&mut self, host: SshHost) {
        if let Some(slot) = self.iter_mut().find(|h| h.alias == host.alias) {
            *slot = host;
        } else {
            self.push(host);
        }
    }

    fn remove_alias(&mut self, alias: &str) -> bool {
        let before = self.len();
        self.retain(|h| h.alias != alias);
        self.len() != before
    }

    fn aliases(&self) -> Vec<String> {
        self.iter().map(|h| h.alias.clone()).collect()
    }
}

// ---------------------------------------------------------------- yollar

/// `~/.evren/ssh_hosts.json`.
pub fn user_hosts_path() -> PathBuf {
    home_dir().join(".evren").join("ssh_hosts.json")
}

/// `<ws>/.evren/ssh_hosts.json`.
pub fn workspace_hosts_path(workspace_root: Option<&Path>) -> PathBuf {
    get_evren_dir(&get_workspace_dir(workspace_root)).join("ssh_hosts.json")
}

/// Proje dosyası varsa o, yoksa kullanıcı dosyası (`/ssh` çıktısı için).
pub fn get_ssh_hosts_path(workspace_root: Option<&Path>) -> PathBuf {
    let local = workspace_hosts_path(workspace_root);
    if local.exists() {
        local
    } else {
        user_hosts_path()
    }
}

/// Dosya adı aynı olan iki yolu Python `resolve()` gibi tekilleştirmek için anahtar.
fn path_key(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

// ---------------------------------------------------------------- okuma/yazma

/// Tek bir kayıt dosyasını okur; dosya yoksa boş.
pub fn read_registry(path: &Path) -> Result<HostMap, EvrenError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| {
        EvrenError::Other(format!(
            "SSH host dosyası okunamadı ({}): {}",
            path.display(),
            e
        ))
    })?;
    let data: Value = serde_json::from_str(&raw).map_err(|e| {
        EvrenError::Other(format!(
            "SSH host dosyası geçersiz JSON içeriyor ({}): {}",
            path.display(),
            e
        ))
    })?;
    let Value::Object(obj) = data else {
        return Err(EvrenError::Other(format!(
            "SSH host dosyası bir nesne ({{alias: {{...}}}}) olmalı: {}",
            path.display()
        )));
    };
    let mut out: HostMap = Vec::new();
    for (alias, item) in obj {
        let Value::Object(m) = item else {
            continue;
        };
        let host = SshHost::from_dict(&alias, &m);
        if host.host.is_empty() {
            continue;
        }
        out.upsert(host);
    }
    Ok(out)
}

/// Kullanıcı + proje kayıtlarını birleştirir (proje kazanır).
pub fn load_hosts(workspace_root: Option<&Path>) -> Result<HostMap, EvrenError> {
    let mut merged: HostMap = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for path in [user_hosts_path(), workspace_hosts_path(workspace_root)] {
        let key = path_key(&path);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        for host in read_registry(&path)? {
            merged.upsert(host);
        }
    }
    Ok(merged)
}

/// Kayıt kapsamı: makine geneli ya da proje.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Workspace,
}

fn scope_path(scope: Scope, workspace_root: Option<&Path>) -> PathBuf {
    match scope {
        Scope::User => user_hosts_path(),
        Scope::Workspace => workspace_hosts_path(workspace_root),
    }
}

/// Python `json.dumps(payload, indent=2, ensure_ascii=False)` çıktısı.
pub fn render_hosts(hosts: &HostMap) -> String {
    let mut payload = Map::new();
    for h in hosts {
        payload.insert(h.alias.clone(), Value::Object(h.to_dict()));
    }
    serde_json::to_string_pretty(&Value::Object(payload)).unwrap_or_else(|_| "{}".into())
}

/// Tek bir kayıt dosyasını yazar.
pub fn save_hosts(
    hosts: &HostMap,
    workspace_root: Option<&Path>,
    scope: Scope,
) -> Result<PathBuf, EvrenError> {
    let path = scope_path(scope, workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EvrenError::io(parent, e))?;
    }
    std::fs::write(&path, render_hosts(hosts)).map_err(|e| EvrenError::io(&path, e))?;
    Ok(path)
}

/// Host ekler ya da günceller (varsayılan kapsam: kullanıcı).
pub fn add_host(
    host: SshHost,
    workspace_root: Option<&Path>,
    scope: Scope,
) -> Result<SshHost, EvrenError> {
    let path = scope_path(scope, workspace_root);
    let mut hosts = read_registry(&path)?;
    hosts.upsert(host.clone());
    save_hosts(&hosts, workspace_root, scope)?;
    Ok(host)
}

/// Alias'ı hangi kayıtta ise oradan siler; silindiyse `true`.
pub fn remove_host(alias: &str, workspace_root: Option<&Path>) -> Result<bool, EvrenError> {
    let mut removed = false;
    for scope in [Scope::Workspace, Scope::User] {
        let path = scope_path(scope, workspace_root);
        let mut hosts = read_registry(&path)?;
        if hosts.remove_alias(alias) {
            save_hosts(&hosts, workspace_root, scope)?;
            removed = true;
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------- geçici hedef

fn adhoc_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    #[allow(clippy::expect_used)]
    RE.get_or_init(|| {
        Regex::new(
            r"^(?:(?P<user>[\w.\-]+)@)?(?P<host>[A-Za-z0-9][A-Za-z0-9.\-]*|\[[0-9a-fA-F:]+\])(?::(?P<port>\d{1,5}))?$",
        )
        .expect("sabit regex")
    })
}

fn ipv4_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    #[allow(clippy::expect_used)]
    RE.get_or_init(|| Regex::new(r"^\d{1,3}(\.\d{1,3}){3}(:\d+)?$").expect("sabit regex"))
}

/// `user@host[:port]` ya da `1.2.3.4[:port]` biçimindeki kayıtsız hedef.
pub fn adhoc_host(spec: &str) -> Option<SshHost> {
    let spec = spec.trim();
    if !spec.contains('@') && !ipv4_re().is_match(spec) {
        return None;
    }
    let caps = adhoc_re().captures(spec)?;
    let port = caps
        .name("port")
        .and_then(|m| m.as_str().parse::<u16>().ok())
        .unwrap_or(22);
    Some(SshHost {
        alias: spec.to_string(),
        host: caps
            .name("host")
            .map(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
        user: caps
            .name("user")
            .map(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
        port,
        identity_file: String::new(),
        description: "kayıtsız (geçici)".into(),
        allow_write: false,
    })
}

/// Kayıtlı alias ya da geçici hedef.
pub fn get_host(alias: &str, workspace_root: Option<&Path>) -> Result<Option<SshHost>, EvrenError> {
    let hosts = load_hosts(workspace_root)?;
    Ok(hosts
        .get_alias(alias)
        .cloned()
        .or_else(|| adhoc_host(alias)))
}

// ---------------------------------------------------------------- öneriler

/// Python `str.casefold()` yaklaşımı (Unicode küçük harf; `ß` → `ss`).
fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == 'ß' {
            out.push_str("ss");
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// `difflib.SequenceMatcher` (junk'sız) eşleşen karakter sayısı.
fn matching_chars(a: &[char], b: &[char]) -> usize {
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, c) in b.iter().enumerate() {
        b2j.entry(*c).or_default().push(j);
    }
    let find_longest = |alo: usize, ahi: usize, blo: usize, bhi: usize| -> (usize, usize, usize) {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, ch) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = b2j.get(ch) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j
                        .checked_sub(1)
                        .and_then(|p| j2len.get(&p))
                        .copied()
                        .unwrap_or(0)
                        + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        (besti, bestj, bestsize)
    };
    let mut queue = vec![(0usize, a.len(), 0usize, b.len())];
    let mut total = 0usize;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = find_longest(alo, ahi, blo, bhi);
        if k > 0 {
            total += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    total
}

/// `difflib.SequenceMatcher(None, a, b).ratio()`.
pub fn sequence_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    2.0 * matching_chars(&a, &b) as f64 / total as f64
}

/// `difflib.get_close_matches(word, possibilities, n, cutoff)`.
pub fn get_close_matches<'a>(
    word: &str,
    possibilities: &[&'a str],
    n: usize,
    cutoff: f64,
) -> Vec<&'a str> {
    let mut scored: Vec<(f64, &'a str)> = possibilities
        .iter()
        .map(|x| (sequence_ratio(x, word), *x))
        .filter(|(r, _)| *r >= cutoff)
        .collect();
    // heapq.nlargest: (ratio, x) çifti azalan sırada.
    scored.sort_by(|l, r| {
        r.0.partial_cmp(&l.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| r.1.cmp(l.1))
    });
    scored.into_iter().take(n).map(|(_, x)| x).collect()
}

/// `alias`'a benzeyen kayıtlı alias'lar (önce gövde eşleşmeleri, sonra bulanık).
pub fn suggest_aliases(alias: &str, hosts: &HostMap, limit: usize) -> Vec<String> {
    let wanted = casefold(alias);
    let names: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
    let mut picked: Vec<String> = names
        .iter()
        .filter(|n| {
            let low = casefold(n);
            wanted.contains(&low) || low.contains(&wanted)
        })
        .map(|n| n.to_string())
        .collect();
    let lowered: Vec<String> = names.iter().map(|n| casefold(n)).collect();
    let lowered_refs: Vec<&str> = lowered.iter().map(String::as_str).collect();
    let close = get_close_matches(&wanted, &lowered_refs, limit, 0.6);
    for n in &names {
        let low = casefold(n);
        if close.contains(&low.as_str()) && !picked.iter().any(|p| p == n) {
            picked.push(n.to_string());
        }
    }
    picked.truncate(limit);
    picked
}

/// Kayıtsız alias için hata metni (kayıtlı seçeneklerle).
pub fn unknown_alias_message(alias: &str, workspace_root: Option<&Path>) -> String {
    let hosts = match load_hosts(workspace_root) {
        Ok(h) => h,
        Err(e) => return format!("HATA: {e}"),
    };
    unknown_alias_message_for(alias, &hosts)
}

/// `unknown_alias_message`'ın verilen kayıtlarla saf sürümü.
pub fn unknown_alias_message_for(alias: &str, hosts: &HostMap) -> String {
    if hosts.is_empty() {
        return format!(
            "HATA: '{alias}' kayıtlı bir SSH host'u değil ve hiç kayıtlı host yok. \
             Kullanıcıdan '/ssh add <alias> <user@host>' ile eklemesini iste."
        );
    }
    let mut names = hosts.aliases();
    names.sort();
    let mut msg = format!(
        "HATA: '{alias}' kayıtlı bir SSH host'u değil. Kayıtlı: {}.",
        names.join(", ")
    );
    let near = suggest_aliases(alias, hosts, 3);
    if !near.is_empty() {
        let details: Vec<String> = near
            .iter()
            .filter_map(|n| hosts.get_alias(n))
            .map(|h| {
                if h.description.is_empty() {
                    format!("{} = {}", h.alias, h.target())
                } else {
                    format!("{} = {} ({})", h.alias, h.target(), h.description)
                }
            })
            .collect();
        msg.push_str(&format!(
            " Benzer: {}. Benzer ad farklı bir makine olabilir: açıklama hedefle açıkça \
             uyuşmuyorsa ask_user ile hangi alias olduğunu sor; uyuşuyorsa onu kullan.",
            details.join("; ")
        ));
    }
    msg
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ratio_matches_difflib() {
        // difflib: SequenceMatcher(None, "koyu3", "koyu3-k8s").ratio() == 0.714285...
        let r = sequence_ratio("koyu3", "koyu3-k8s");
        assert!((r - 10.0 / 14.0).abs() < 1e-9);
        assert!((sequence_ratio("abcd", "bcde") - 0.75).abs() < 1e-9);
        assert!((sequence_ratio("", "") - 1.0).abs() < 1e-9);
        assert!((sequence_ratio("abc", "xyz")).abs() < 1e-9);
    }

    #[test]
    fn close_matches_order() {
        let got = get_close_matches("appel", &["ape", "apple", "peach", "puppy"], 3, 0.6);
        assert_eq!(got, vec!["apple", "ape"]);
    }

    #[test]
    fn suggest_and_message() {
        let mut hosts: HostMap = Vec::new();
        let mut a = SshHost::new("koyu1-k8s", "10.0.0.1");
        a.user = "root".into();
        hosts.upsert(a);
        hosts.upsert(SshHost::new("koyu2-k8s", "10.0.0.2"));
        let mut c = SshHost::new("koyu3", "10.0.0.3");
        c.description = "üretim".into();
        hosts.upsert(c);
        let near = suggest_aliases("koyu3-k8s", &hosts, 3);
        assert_eq!(near[0], "koyu3");
        let msg = unknown_alias_message_for("koyu3-k8s", &hosts);
        assert!(msg.starts_with(
            "HATA: 'koyu3-k8s' kayıtlı bir SSH host'u değil. Kayıtlı: koyu1-k8s, koyu2-k8s, koyu3."
        ));
        assert!(msg.contains("koyu3 = 10.0.0.3 (üretim)"));
        assert!(unknown_alias_message_for("x", &Vec::new()).contains("hiç kayıtlı host yok"));
    }

    #[test]
    fn adhoc() {
        let h = adhoc_host("root@1.2.3.4:2222").unwrap();
        assert_eq!(
            (h.user.as_str(), h.host.as_str(), h.port),
            ("root", "1.2.3.4", 2222)
        );
        assert_eq!(h.description, "kayıtsız (geçici)");
        assert_eq!(adhoc_host("10.0.0.3").unwrap().target(), "10.0.0.3");
        assert!(adhoc_host("koyu3").is_none());
        assert!(adhoc_host("a@b c").is_none());
    }

    #[test]
    fn from_dict_coercions() {
        let m: Map<String, Value> =
            serde_json::from_str(r#"{"host": " h ", "port": "0", "allow_write": 1, "user": null}"#)
                .unwrap();
        let h = SshHost::from_dict("x", &m);
        assert_eq!(h.host, "h");
        assert_eq!(h.port, 22);
        assert!(h.allow_write);
        assert_eq!(h.user, "None"); // Python str(None)
    }
}
