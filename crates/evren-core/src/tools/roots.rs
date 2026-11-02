//! Adlı çalışma kökleri (`--root ad=yol`, tekrarlanabilir; Python `roots.py`).
//!
//! Bir görev birden çok depoya yayılabilir. Her ek köke kısa bir ad verilir;
//! araçlar ona `ad:göreli/yol` önekiyle ya da dizin kapsamlı araçlarda `root`
//! argümanıyla ulaşır. Ana çalışma dizini varsayılan köktür. Adlar en az iki
//! karakterdir; Windows sürücü harfi (`C:/x`) kök sanılmaz.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use regex::Regex;
use serde_json::{Map, Value};

use super::file_ops::{resolve_path, resolve_workspace_path};

/// `root` argümanı çalışma dizinini seçen araçlar.
pub const ROOT_ARG_TOOLS: [&str; 5] = [
    "run_command",
    "find_files",
    "list_directory",
    "search_code",
    "start_background",
];
/// Çalışma dizini yolu taşıyan argüman anahtarları.
const PATH_KEYS: [&str; 6] = ["path", "dest", "target", "source", "src", "file"];

fn name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    #[allow(clippy::unwrap_used)]
    RE.get_or_init(|| Regex::new(r"^[A-Za-z][A-Za-z0-9_.-]+$").unwrap())
}

fn patch_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    #[allow(clippy::unwrap_used)]
    RE.get_or_init(|| {
        Regex::new(r"(?m)^(\*\*\* (?:Add|Update|Delete) File:\s*|\*\*\* Move to:\s*)(\S[^\r\n]*?)\s*$")
            .unwrap()
    })
}

fn registry() -> &'static RwLock<Vec<(String, PathBuf)>> {
    static ROOTS: OnceLock<RwLock<Vec<(String, PathBuf)>>> = OnceLock::new();
    ROOTS.get_or_init(|| RwLock::new(Vec::new()))
}

thread_local! {
    static OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Bu iş parçacığındaki araç çağrısı için seçilen kök (yoksa `None`).
pub fn current_override() -> Option<PathBuf> {
    OVERRIDE.with(|o| o.borrow().clone())
}

/// Bırakılınca kök seçimini geri alan bekçi.
pub struct RootOverride(Option<PathBuf>);

impl RootOverride {
    pub fn set(root: Option<PathBuf>) -> Self {
        RootOverride(OVERRIDE.with(|o| o.replace(root)))
    }
}

impl Drop for RootOverride {
    fn drop(&mut self) {
        let prev = self.0.take();
        OVERRIDE.with(|o| *o.borrow_mut() = prev);
    }
}

/// `ad=yol` → `(ad, mutlak yol)`.
pub fn parse_root_spec(spec: &str) -> Result<(String, PathBuf), String> {
    let (name, path) = match spec.split_once('=') {
        Some((n, p)) if !p.trim().is_empty() => (n.trim(), p.trim()),
        _ => return Err(format!("--root biçimi 'ad=yol' olmalı: {spec}")),
    };
    if !name_re().is_match(name) {
        return Err(format!(
            "Geçersiz kök adı '{name}' (harfle başlar, en az 2 karakter)."
        ));
    }
    let expanded = match (path.strip_prefix('~'), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(path),
    };
    let p = resolve_path(&expanded);
    if !p.is_dir() {
        return Err(format!("Kök dizini yok: {}", p.display()));
    }
    Ok((name.to_string(), p))
}

/// Kayıt defterini değiştirir.
pub fn set_roots(specs: &[String]) -> Result<Vec<(String, PathBuf)>, String> {
    let mut parsed = Vec::new();
    for spec in specs {
        let (name, path) = parse_root_spec(spec)?;
        parsed.retain(|(n, _): &(String, PathBuf)| *n != name);
        parsed.push((name, path));
    }
    if let Ok(mut reg) = registry().write() {
        *reg = parsed.clone();
    }
    Ok(parsed)
}

/// Tanımlı ek kökler (sırasıyla).
pub fn roots() -> Vec<(String, PathBuf)> {
    registry().read().map(|r| r.clone()).unwrap_or_default()
}

fn lookup(roots: &[(String, PathBuf)], name: &str) -> Option<PathBuf> {
    roots.iter().find(|(n, _)| n == name).map(|(_, p)| p.clone())
}

/// `api:src/x.py` → `("api", "src/x.py")` ('api' kayıtlıysa).
pub fn split_prefix_in(roots: &[(String, PathBuf)], path: &str) -> Option<(String, String)> {
    if roots.is_empty() {
        return None;
    }
    let (name, rest) = path.split_once(':')?;
    lookup(roots, name)?;
    let rest = rest.trim_start_matches(['/', '\\']);
    Some((
        name.to_string(),
        if rest.is_empty() { "." } else { rest }.to_string(),
    ))
}

/// Kök önekini dikkate alarak araç yolunun mutlak biçimi.
pub fn resolve(path: &str, default_root: &Path) -> PathBuf {
    let roots = roots();
    if let Some((name, rest)) = split_prefix_in(&roots, path) {
        if let Some(root) = lookup(&roots, &name) {
            return resolve_workspace_path(&rest, &root);
        }
    }
    resolve_workspace_path(path, default_root)
}

/// Yolu tutan köke göre göreli (posix) biçim; hiçbirinde değilse mutlak.
pub fn relative(path: &Path, default_root: &Path) -> String {
    let path = resolve_path(path);
    let mut bases = vec![resolve_path(default_root)];
    bases.extend(roots().into_iter().map(|(_, p)| p));
    for base in bases {
        if let Ok(rel) = path.strip_prefix(&base) {
            let s = rel.to_string_lossy().replace('\\', "/");
            return if s.is_empty() { ".".to_string() } else { s };
        }
    }
    path.to_string_lossy().replace('\\', "/")
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Kök öneklerini / `root` argümanını ayıklar: `(argümanlar, seçilen kök, hata)`.
/// Seçilen kök `None` ise varsayılan kök kullanılır.
pub fn route_in(
    roots: &[(String, PathBuf)],
    name: &str,
    arguments: Value,
) -> (Value, Option<PathBuf>, String) {
    let Value::Object(mut args) = arguments else {
        return (arguments, None, String::new());
    };
    let chosen = args.remove("root").and_then(|v| scalar(&v));
    let is_main = |c: &Option<String>| matches!(c.as_deref(), None | Some("" | "main" | "."));
    if roots.is_empty() {
        if !is_main(&chosen) {
            let c = chosen.unwrap_or_default();
            return (
                Value::Object(args),
                None,
                format!("HATA: '{c}' adlı kök yok (bu oturumda ek kök tanımlı değil)."),
            );
        }
        return (Value::Object(args), None, String::new());
    }
    let mut picked: Vec<String> = Vec::new();
    let pick = |n: String, picked: &mut Vec<String>| {
        if !picked.contains(&n) {
            picked.push(n);
        }
    };
    if !is_main(&chosen) {
        let c = chosen.unwrap_or_default();
        if lookup(roots, &c).is_none() {
            let names: Vec<&str> = roots.iter().map(|(n, _)| n.as_str()).collect();
            return (
                Value::Object(args),
                None,
                format!(
                    "HATA: '{c}' adlı kök yok (tanımlı: main, {}).",
                    names.join(", ")
                ),
            );
        }
        pick(c, &mut picked);
    }
    for key in PATH_KEYS {
        if let Some(Value::String(val)) = args.get(key) {
            if let Some((root, rest)) = split_prefix_in(roots, val) {
                pick(root, &mut picked);
                args.insert(key.to_string(), Value::String(rest));
            }
        }
    }
    if name == "apply_patch" {
        if let Some(Value::String(patch)) = args.get("patch") {
            let mut hits: Vec<String> = Vec::new();
            let replaced = patch_path_re()
                .replace_all(patch, |c: &regex::Captures| {
                    match split_prefix_in(roots, &c[2]) {
                        Some((root, rest)) => {
                            hits.push(root);
                            format!("{}{rest}", &c[1])
                        }
                        None => c[0].to_string(),
                    }
                })
                .into_owned();
            for h in hits {
                pick(h, &mut picked);
            }
            args.insert("patch".into(), Value::String(replaced));
        }
    }
    if picked.len() > 1 {
        picked.sort();
        return (
            Value::Object(args),
            None,
            format!(
                "HATA: Tek çağrı tek köke gider; birden çok kök verildi: {}. Çağrıyı kök başına böl.",
                picked.join(", ")
            ),
        );
    }
    let root = picked.pop().and_then(|n| lookup(roots, &n));
    (Value::Object(args), root, String::new())
}

/// Kayıtlı köklerle [`route_in`].
pub fn route(name: &str, arguments: Value) -> (Value, Option<PathBuf>, String) {
    route_in(&roots(), name, arguments)
}

/// Kök varsa dizin kapsamlı araçlara isteğe bağlı `root` enum'u ekler.
pub fn schema_with_roots(tools: Vec<Value>) -> Vec<Value> {
    let roots = roots();
    if roots.is_empty() {
        return tools;
    }
    let mut names = vec![Value::String("main".into())];
    names.extend(roots.iter().map(|(n, _)| Value::String(n.clone())));
    tools
        .into_iter()
        .map(|mut t| {
            let fname = t
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if ROOT_ARG_TOOLS.contains(&fname.as_str()) {
                if let Some(func) = t.get_mut("function").and_then(Value::as_object_mut) {
                    let params = func
                        .entry("parameters")
                        .or_insert_with(|| serde_json::json!({"type": "object", "properties": {}}));
                    if let Some(params) = params.as_object_mut() {
                        let props = params
                            .entry("properties")
                            .or_insert_with(|| Value::Object(Map::new()));
                        if let Some(props) = props.as_object_mut() {
                            props.insert(
                                "root".into(),
                                serde_json::json!({
                                    "type": "string",
                                    "enum": names.clone(),
                                    "description": "Çalışılacak kök (varsayılan main).",
                                }),
                            );
                        }
                    }
                }
            }
            t
        })
        .collect()
}

/// Sistem iletisi için kök notu; kök yoksa boş.
pub fn prompt_note(default_root: &Path) -> String {
    let roots = roots();
    let Some((first, _)) = roots.first() else {
        return String::new();
    };
    let mut lines = vec![
        "[KÖKLER] Bu görev birden çok depoda çalışır:".to_string(),
        format!(
            "- main: {} (varsayılan)",
            resolve_path(default_root).display()
        ),
    ];
    lines.extend(roots.iter().map(|(n, p)| format!("- {n}: {}", p.display())));
    lines.push(format!(
        "Dosya yollarında 'ad:göreli/yol' kullan (örn. {first}:src/app.py); run_command/find_files/list_directory/search_code için root=\"ad\" ver."
    ));
    lines.join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reg(dir: &Path) -> Vec<(String, PathBuf)> {
        vec![("api".to_string(), resolve_path(dir))]
    }

    #[test]
    fn spec_parsing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(parse_root_spec("x").is_err());
        assert!(parse_root_spec("a=.").is_err());
        assert!(parse_root_spec("api=/yok/yok/yok").is_err());
        let (n, p) = parse_root_spec(&format!("api={}", dir.path().display()))
            .unwrap();
        assert_eq!(n, "api");
        assert!(p.is_dir());
    }

    #[test]
    fn routing() {
        let dir = tempfile::tempdir().unwrap();
        let roots = reg(dir.path());
        let (_, root, err) = route_in(&[], "run_command", json!({"command": "x", "root": "api"}));
        assert!(root.is_none());
        assert!(err.contains("ek kök tanımlı değil"));
        let (args, root, err) = route_in(&roots, "view_file", json!({"path": "api:src/a.py"}));
        assert!(err.is_empty());
        assert_eq!(args["path"], "src/a.py");
        assert_eq!(root, Some(resolve_path(dir.path())));
        let (args, root, _) = route_in(&roots, "run_command", json!({"command": "x", "root": "main"}));
        assert!(root.is_none());
        assert!(args.get("root").is_none());
        let (_, _, err) = route_in(&roots, "run_command", json!({"command": "x", "root": "web"}));
        assert!(err.contains("tanımlı: main, api"));
        let (args, root, _) = route_in(
            &roots,
            "apply_patch",
            json!({"patch": "*** Begin Patch\n*** Update File: api:src/a.py\n@@\n*** End Patch"}),
        );
        assert!(root.is_some());
        assert!(args["patch"].as_str().unwrap_or("").contains("File: src/a.py"));
        // Sürücü harfi kök sayılmaz.
        assert!(split_prefix_in(&roots, "C:/x").is_none());
    }
}
