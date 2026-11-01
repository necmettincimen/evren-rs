//! Oturum içi yapılacaklar listesi (Python `todos.py`).

use serde_json::{json, Value};

pub const VALID_STATUSES: [&str; 3] = ["pending", "in_progress", "completed"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    pub content: String,
    pub status: String,
}

#[derive(Debug, Default, Clone)]
pub struct TodoStore {
    items: Vec<Todo>,
    /// Başarılı `write` sayısı (boşta kalma notu için).
    pub writes: u32,
}

fn marker(status: &str) -> &'static str {
    match status {
        "in_progress" => "~",
        "completed" => "x",
        _ => " ",
    }
}

impl TodoStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Listeyi doğrular ve değiştirir; hata olursa eski liste korunur.
    pub fn write(&mut self, raw: &[Value]) -> Result<(), String> {
        let mut items = Vec::with_capacity(raw.len());
        let mut in_progress = 0;
        for item in raw {
            let Some(obj) = item.as_object() else {
                return Err("Her todo maddesi {content, status} alanlı bir nesne olmalı.".into());
            };
            let content = value_text(obj.get("content")).trim().to_string();
            let status = match obj.get("status").filter(|v| !v.is_null()) {
                Some(v) => value_text(Some(v)),
                None => "pending".to_string(),
            };
            let status = status.trim().to_lowercase();
            if content.is_empty() {
                return Err("Todo maddesinin 'content' alanı boş olamaz.".into());
            }
            if !VALID_STATUSES.contains(&status.as_str()) {
                return Err(format!(
                    "Geçersiz durum: '{status}'. İzin verilenler: pending, in_progress, completed."
                ));
            }
            if status == "in_progress" {
                in_progress += 1;
            }
            items.push(Todo { content, status });
        }
        if in_progress > 1 {
            return Err(format!(
                "Aynı anda yalnızca bir madde in_progress olabilir ({in_progress} verildi)."
            ));
        }
        self.items = items;
        self.writes += 1;
        Ok(())
    }

    /// Kullanıcıya gösterilen liste; boşsa boş dize.
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let done = self
            .items
            .iter()
            .filter(|t| t.status == "completed")
            .count();
        let mut lines = vec![format!(
            "Görev listesi ({done}/{} tamamlandı):",
            self.items.len()
        )];
        for t in &self.items {
            lines.push(format!(
                "[{}] {} ({})",
                marker(&t.status),
                t.content,
                t.status
            ));
        }
        lines.join("\n")
    }

    pub fn snapshot(&self) -> Vec<Value> {
        self.items
            .iter()
            .map(|t| json!({"content": t.content, "status": t.status}))
            .collect()
    }

    pub fn items(&self) -> &[Todo] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// Python `str(value)` benzeri: dize ise aynen, aksi halde JSON gösterimi.
fn value_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn write_and_render() {
        let mut s = TodoStore::new();
        s.write(&[
            json!({"content": " a ", "status": "in_progress"}),
            json!({"content": "b"}),
        ])
        .unwrap();
        assert_eq!(
            s.render(),
            "Görev listesi (0/2 tamamlandı):\n[~] a (in_progress)\n[ ] b (pending)"
        );
        assert_eq!(s.writes, 1);
    }

    #[test]
    fn errors_keep_old_list() {
        let mut s = TodoStore::new();
        s.write(&[json!({"content": "a"})]).unwrap();
        let err = s.write(&[json!({"content": "", "status": "pending"})]);
        assert_eq!(
            err,
            Err("Todo maddesinin 'content' alanı boş olamaz.".into())
        );
        let err = s.write(&[json!({"content": "x", "status": "done"})]);
        assert_eq!(
            err,
            Err("Geçersiz durum: 'done'. İzin verilenler: pending, in_progress, completed.".into())
        );
        let err = s.write(&[
            json!({"content": "x", "status": "in_progress"}),
            json!({"content": "y", "status": "in_progress"}),
        ]);
        assert_eq!(
            err,
            Err("Aynı anda yalnızca bir madde in_progress olabilir (2 verildi).".into())
        );
        assert_eq!(s.items().len(), 1);
        assert_eq!(s.writes, 1);
    }
}
