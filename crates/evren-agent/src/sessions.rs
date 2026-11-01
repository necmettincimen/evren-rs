//! Oturum kaydı ve geri yükleme (`.evren/sessions/<damga>.json`), Python
//! `sessions.py` paritesi. Dosya Python `json.dumps(ensure_ascii=False)`
//! ayraçlarıyla (`", "` / `": "`) yazılır; Python'un yazdığı dosya burada,
//! buradan yazılan dosya Python'da aynı şekilde okunur.

use std::io;
use std::path::{Path, PathBuf};

use evren_core::config::get_evren_dir;
use serde::Serialize;
use serde_json::ser::Formatter;
use serde_json::{json, Map, Value};

use crate::session::{py_scalar, AgentSession};

/// `<ws>/.evren/sessions`.
pub fn sessions_dir(workspace_root: &Path) -> PathBuf {
    get_evren_dir(workspace_root).join("sessions")
}

/// En yeni önce, en çok `limit` dosya (`sorted(glob("*.json"), reverse=True)`).
pub fn list_sessions(workspace_root: &Path, limit: usize) -> Vec<PathBuf> {
    let dir = sessions_dir(workspace_root);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json") && p.is_file())
        .collect();
    paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    paths.truncate(limit);
    paths
}

/// Dosya adı gövdesi (`20260101-120000`).
pub fn session_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Dosyadan okunan `title` (okunamazsa boş).
pub fn session_title(path: &Path) -> String {
    read_session_file(path)
        .and_then(|d| d.get("title").map(py_scalar))
        .unwrap_or_default()
}

/// stdio `sessions` olayı için özet: `{session_id, title, model, mode}`.
pub fn session_summary(path: &Path) -> Value {
    let data = read_session_file(path).unwrap_or_default();
    let field = |k: &str| data.get(k).map(py_scalar).unwrap_or_default();
    json!({
        "session_id": session_id(path),
        "title": field("title"),
        "model": field("model"),
        "mode": field("mode"),
    })
}

fn read_session_file(path: &Path) -> Option<Map<String, Value>> {
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Value>(&raw).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// `time.strftime("%Y%m%d-%H%M%S")` (yerel saat).
pub fn new_stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Python `json.dumps` varsayılan ayraçları: `", "` ve `": "`, satır sonu yok.
struct PyCompact;

impl Formatter for PyCompact {
    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        w.write_all(b": ")
    }
}

/// Değeri Python `json.dumps(obj, ensure_ascii=False)` biçiminde yazar.
pub fn dumps_py(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PyCompact);
    if value.serialize(&mut ser).is_err() {
        return String::from("null");
    }
    String::from_utf8(buf).unwrap_or_else(|_| String::from("null"))
}

/// Python `first.strip().splitlines()[0][:80]`.
fn title_of(first: &str) -> String {
    let t = first.trim();
    if t.is_empty() {
        return String::new();
    }
    t.lines().next().unwrap_or("").chars().take(80).collect()
}

impl AgentSession {
    /// Oturumu `.evren/sessions/<damga>.json` dosyasına yazar; sistem dışı
    /// mesaj yoksa ya da yazılamazsa `None`.
    pub fn save_session(&mut self) -> Option<PathBuf> {
        let msgs: Vec<Value> = self
            .messages
            .iter()
            .filter(|m| m.get("role").and_then(Value::as_str) != Some("system"))
            .cloned()
            .collect();
        if msgs.is_empty() {
            return None;
        }
        let stamp = match &self.session_stamp {
            Some(s) if !s.is_empty() => s.clone(),
            _ => {
                let s = new_stamp();
                self.session_stamp = Some(s.clone());
                s
            }
        };
        let first = msgs
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
            .map(|m| match m.get("content") {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(s)) if s.is_empty() => String::new(),
                Some(v) => py_scalar(v),
            })
            .unwrap_or_default();
        let data = json!({
            "model": self.model,
            "mode": self.mode.as_str(),
            "goal": self.task_goal,
            "title": title_of(&first),
            "messages": msgs,
        });
        let masked = self.mask_value(&data);
        let dir = sessions_dir(&self.workspace_root);
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let path = dir.join(format!("{stamp}.json"));
        std::fs::write(&path, dumps_py(&masked)).ok()?;
        Some(path)
    }

    /// Python `_mask`: JSON metni kasa ile maskelenip geri çözümlenir; hata → özgün değer.
    fn mask_value(&self, value: &Value) -> Value {
        if self.vault.is_empty() {
            return value.clone();
        }
        let masked = self.vault.mask(&dumps_py(value));
        serde_json::from_str(&masked).unwrap_or_else(|_| value.clone())
    }

    /// Kayıtlı oturumu yükler: sistem iletisi korunur, mesajlar eklenir,
    /// `goal` boş değilse görev hedefi olur, damga dosya adından alınır.
    pub fn load_session(&mut self, path: &Path) -> bool {
        let Some(data) = read_session_file(path) else {
            return false;
        };
        let Some(Value::Array(msgs)) = data.get("messages").cloned() else {
            return false;
        };
        if let Some(first) = self.messages.first().cloned() {
            let mut all = Vec::with_capacity(msgs.len() + 1);
            all.push(first);
            all.extend(msgs);
            self.messages = all;
        } else {
            self.messages = msgs;
        }
        if let Some(goal) = data.get("goal") {
            let g = match goal {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => py_scalar(other),
            };
            if !g.is_empty() {
                self.task_goal = g;
            }
        }
        self.session_stamp = Some(session_id(path));
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn python_separators() {
        let v = json!({"a": [1, "ç", null, true], "b": {"c": 1.5}});
        assert_eq!(
            dumps_py(&v),
            r#"{"a": [1, "ç", null, true], "b": {"c": 1.5}}"#
        );
        assert_eq!(dumps_py(&json!({})), "{}");
        assert_eq!(dumps_py(&json!([])), "[]");
    }

    #[test]
    fn titles_and_listing() {
        assert_eq!(title_of("  \n ilk satır \n ikinci"), "ilk satır "); // strip() yalnız uçları kırpar
        assert_eq!(title_of("  \n  "), "");
        let long = "x".repeat(100);
        assert_eq!(title_of(&long).len(), 80);

        let dir = tempfile::tempdir().unwrap();
        let sd = sessions_dir(dir.path());
        assert!(list_sessions(dir.path(), 10).is_empty());
        std::fs::create_dir_all(&sd).unwrap();
        for n in ["20260101-120000", "20260102-080000", "20251231-235959"] {
            std::fs::write(
                sd.join(format!("{n}.json")),
                r#"{"title": "t", "messages": []}"#,
            )
            .unwrap();
        }
        std::fs::write(sd.join("notes.txt"), "x").unwrap();
        let got: Vec<String> = list_sessions(dir.path(), 2)
            .iter()
            .map(|p| session_id(p))
            .collect();
        assert_eq!(got, vec!["20260102-080000", "20260101-120000"]);
        let s = session_summary(&sd.join("20260101-120000.json"));
        assert_eq!(s["session_id"], "20260101-120000");
        assert_eq!(s["title"], "t");
        assert_eq!(s["model"], "");
    }
}
