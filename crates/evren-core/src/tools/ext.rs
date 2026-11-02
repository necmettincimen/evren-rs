//! Eklenti araçları (Python `tool_ext.py`): isteğe bağlı `web_search` ve MCP araçları.

use std::path::Path;
use std::sync::LazyLock;

use serde_json::{json, Value};

use crate::config::SslVerify;
use crate::settings::Settings;

static WEB_SEARCH_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "function",
        "function": {
            "name": "web_search",
            "description": "Yapılandırılmış arama ucuyla web'de arar; başlık/url/özet listesi döner.",
            "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]},
        },
    })
});

fn web_search_url(root: &Path) -> String {
    Settings::load(Some(root))
        .data
        .get("webSearch")
        .and_then(|w| w.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Normal kipte araç listesine eklenen şemalar: `webSearch.url` varsa `web_search`,
/// ardından tanımlı MCP sunucularının araçları.
pub fn extra_schemas(root: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    if !web_search_url(root).is_empty() {
        out.push(WEB_SEARCH_SCHEMA.clone());
    }
    out.extend(super::mcp::schemas(root));
    out
}

/// Python `urllib.parse.quote` (güvenli: harf, rakam, `_.-~/`).
fn quote(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Sonuç listesini `- başlık | url | özet` satırlarına çevirir (en çok 8).
fn format_results(data: &Value) -> String {
    let empty = Vec::new();
    let items = match data {
        Value::Array(a) => a,
        _ => ["results", "items"]
            .iter()
            .filter_map(|k| data.get(*k).and_then(Value::as_array))
            .chain(data.pointer("/web/results").and_then(Value::as_array))
            .find(|a| !a.is_empty())
            .unwrap_or(&empty),
    };
    let field = |it: &Value, keys: &[&str]| -> String {
        keys.iter()
            .filter_map(|k| it.get(*k))
            .find(|v| !v.is_null() && v.as_str() != Some(""))
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
            .unwrap_or_default()
    };
    let lines: Vec<String> = items
        .iter()
        .take(8)
        .filter(|it| it.is_object())
        .map(|it| {
            let snippet: String = field(it, &["snippet", "content", "description"])
                .chars()
                .take(200)
                .collect();
            format!(
                "- {} | {} | {snippet}",
                field(it, &["title"]),
                field(it, &["url", "link"])
            )
        })
        .collect();
    if lines.is_empty() {
        "Sonuç yok.".to_string()
    } else {
        lines.join("\n")
    }
}

/// `webSearch.url` (`{q}` yer tutuculu) ucuna sorgu gönderir.
pub fn web_search(query: &str, root: &Path, ssl: &SslVerify) -> String {
    let url = web_search_url(root);
    if url.is_empty() {
        return "HATA: webSearch.url ayarlı değil.".to_string();
    }
    let url = url.replace("{q}", &quote(query));
    let data = super::web_fetch::build_client(20, ssl)
        .and_then(|c| c.get(&url).send().map_err(|e| e.to_string()))
        .and_then(|r| r.text().map_err(|e| e.to_string()))
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()));
    match data {
        Ok(d) => format_results(&d),
        Err(e) => format!("HATA: arama başarısız: {e}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn quote_matches_python() {
        assert_eq!(quote("a b/ç?"), "a%20b/%C3%A7%3F");
    }

    #[test]
    fn results_from_known_shapes() {
        let list = json!([{"title": "T", "link": "u", "content": "c"}]);
        assert_eq!(format_results(&list), "- T | u | c");
        let brave = json!({"web": {"results": [{"title": "B", "url": "v", "description": "d"}]}});
        assert_eq!(format_results(&brave), "- B | v | d");
        assert_eq!(format_results(&json!({"results": []})), "Sonuç yok.");
    }

    #[test]
    fn web_search_needs_url() {
        let dir = tempfile::tempdir().unwrap();
        let out = web_search("x", dir.path(), &SslVerify::Flag(true));
        // Ev dizininde webSearch.url tanımlı olabilir; tanımlı değilse açık hata döner.
        if web_search_url(dir.path()).is_empty() {
            assert_eq!(out, "HATA: webSearch.url ayarlı değil.");
        }
    }
}
