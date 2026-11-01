//! Araç çağrısı argümanlarını toleranslı çözümleme (Python `tool_args.py`):
//! çift kodlanmış JSON, yanlış tip, takma ad anahtarlar.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

/// Takma ad -> kanonik parametre eşlemesi. `*` her araca uygulanır.
/// Her girdi: (araç, kanonik, [(takma ad, kanonik)]).
pub const PARAMETER_ALIASES: &[(&str, &str, &[&str])] = &[
    (
        "edit_file",
        "target_snippet",
        &[
            "old_text",
            "old_string",
            "old_str",
            "original_text",
            "find",
            "search",
            "search_text",
            "context",
            "target",
            "old",
        ],
    ),
    ("*", "path", &["file_path", "filename", "file"]),
    ("*", "command", &["cmd", "shell"]),
];

/// `edit_file.edits` öğelerine uygulanan takma adlar.
pub const EDIT_ITEM_ALIASES: &[(&str, &str)] = &[
    ("old_text", "target_snippet"),
    ("old_string", "target_snippet"),
    ("old_str", "target_snippet"),
    ("original_text", "target_snippet"),
    ("find", "target_snippet"),
    ("search", "target_snippet"),
    ("search_text", "target_snippet"),
    ("old", "target_snippet"),
    ("new_text", "replacement_snippet"),
    ("new_string", "replacement_snippet"),
    ("new_str", "replacement_snippet"),
    ("replacement", "replacement_snippet"),
    ("replace", "replacement_snippet"),
    ("new", "replacement_snippet"),
    ("text", "replacement_snippet"),
    ("replace_all", "all"),
    ("replaceAll", "all"),
];

const TRUE_STRINGS: &[&str] = &["true", "yes", "y", "1", "on"];
const FALSE_STRINGS: &[&str] = &["false", "no", "n", "0", "off", ""];

/// Argümanlar kullanılabilir biçime getirilemedi.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ToolArgumentError(pub String);

/// `{araç: {parametre: json tipi}}`.
pub type TypeMap = HashMap<String, HashMap<String, String>>;
/// `{araç: [zorunlu parametre]}`.
pub type RequiredMap = HashMap<String, Vec<String>>;

/// Şemadan tip haritası üretir.
pub fn schema_type_map(tools_schema: &[Value]) -> TypeMap {
    let mut mapping = TypeMap::new();
    for entry in tools_schema {
        let Some(function) = entry.get("function") else {
            continue;
        };
        let Some(name) = function.get("name").and_then(Value::as_str) else {
            continue;
        };
        let mut types = HashMap::new();
        if let Some(props) = function
            .get("parameters")
            .and_then(|p| p.get("properties"))
            .and_then(Value::as_object)
        {
            for (param, spec) in props {
                if let Some(t) = spec.get("type").and_then(Value::as_str) {
                    types.insert(param.clone(), t.to_string());
                }
            }
        }
        mapping.insert(name.to_string(), types);
    }
    mapping
}

/// Şemadan zorunlu parametre haritası üretir.
pub fn schema_required_map(tools_schema: &[Value]) -> RequiredMap {
    let mut mapping = RequiredMap::new();
    for entry in tools_schema {
        let Some(function) = entry.get("function") else {
            continue;
        };
        let Some(name) = function.get("name").and_then(Value::as_str) else {
            continue;
        };
        let required = function
            .get("parameters")
            .and_then(|p| p.get("required"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        mapping.insert(name.to_string(), required);
    }
    mapping
}

/// Python `repr()` yaklaşımı (hata mesajlarında).
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_repr_str(s),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `type(value).__name__`.
pub fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn looks_like_json(value: &str) -> bool {
    let stripped = value.trim();
    if stripped.chars().count() < 2 {
        return false;
    }
    stripped.starts_with('[')
        || stripped.starts_with('{')
        || matches!(stripped, "null" | "true" | "false")
}

/// JSON metnini çözer; hatada açıklayıcı `ToolArgumentError`.
pub fn load_json_string(value: &str, field: &str) -> Result<Value, ToolArgumentError> {
    serde_json::from_str(value).map_err(|exc| {
        ToolArgumentError(format!(
            "'{field}' parametresi JSON dizisi/nesnesi olarak çözümlenemedi ({exc}). Beklenen: geçerli bir JSON. Gönderilen: {}",
            py_repr_str(&truncate_chars(value, 200))
        ))
    })
}

/// Tek değeri şemada bildirilen tipe dönüştürür.
pub fn coerce_value(
    value: Value,
    expected_type: &str,
    field: &str,
) -> Result<Value, ToolArgumentError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    match expected_type {
        "array" | "object" => coerce_structure(value, expected_type, field),
        "string" => Ok(Value::String(match &value {
            Value::String(s) => s.clone(),
            Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        })),
        "boolean" => match &value {
            Value::Bool(_) => Ok(value),
            Value::String(s) => {
                let lowered = s.trim().to_lowercase();
                if TRUE_STRINGS.contains(&lowered.as_str()) {
                    Ok(Value::Bool(true))
                } else if FALSE_STRINGS.contains(&lowered.as_str()) {
                    Ok(Value::Bool(false))
                } else {
                    Err(ToolArgumentError(format!(
                        "'{field}' boolean olmalı, geçersiz değer: {}. Beklenen: true/false.",
                        py_repr(&value)
                    )))
                }
            }
            Value::Number(n) => Ok(Value::Bool(n.as_f64().map(|f| f != 0.0).unwrap_or(false))),
            _ => Err(ToolArgumentError(format!(
                "'{field}' boolean olmalı, geçersiz değer: {}.",
                py_repr(&value)
            ))),
        },
        "integer" | "number" => match &value {
            Value::Bool(_) => Err(ToolArgumentError(format!(
                "'{field}' sayı olmalı, geçersiz değer: {}.",
                py_repr(&value)
            ))),
            Value::Number(n) => Ok(number_as(n.as_f64().unwrap_or(0.0), n, expected_type)),
            Value::String(s) => {
                let text = s.trim();
                if text.is_empty() {
                    return Ok(Value::Null);
                }
                if expected_type == "integer" {
                    if let Ok(i) = text.parse::<i64>() {
                        return Ok(Value::from(i));
                    }
                } else if let Ok(f) = text.parse::<f64>() {
                    return Ok(Value::from(f));
                }
                let Ok(number) = text.parse::<f64>() else {
                    return Err(ToolArgumentError(format!(
                        "'{field}' {expected_type} olmalı, geçersiz değer: {}.",
                        py_repr(&value)
                    )));
                };
                if expected_type == "integer" {
                    if number.fract() != 0.0 {
                        return Err(ToolArgumentError(format!(
                            "'{field}' tam sayı olmalı, ondalıklı değer verildi: {}.",
                            py_repr(&value)
                        )));
                    }
                    return Ok(Value::from(number as i64));
                }
                Ok(Value::from(number))
            }
            _ => Err(ToolArgumentError(format!(
                "'{field}' {expected_type} olmalı, geçersiz değer: {}.",
                py_repr(&value)
            ))),
        },
        _ => Ok(value),
    }
}

fn number_as(f: f64, n: &serde_json::Number, expected_type: &str) -> Value {
    if expected_type == "integer" {
        if let Some(i) = n.as_i64() {
            Value::from(i)
        } else {
            Value::from(f as i64)
        }
    } else {
        Value::from(f)
    }
}

fn coerce_structure(
    mut value: Value,
    expected_type: &str,
    field: &str,
) -> Result<Value, ToolArgumentError> {
    if let Value::String(s) = &value {
        if s.trim().is_empty() {
            return Ok(Value::Null);
        }
        if !looks_like_json(s) {
            let expected = if expected_type == "array" {
                "bir JSON dizisi"
            } else {
                "bir JSON nesnesi"
            };
            return Err(ToolArgumentError(format!(
                "'{field}' parametresi JSON olarak çözümlenemeyen düz metin: {}. Beklenen: {expected}.",
                py_repr_str(&truncate_chars(s, 200))
            )));
        }
        value = load_json_string(s, field)?;
    }
    if expected_type == "array" {
        return match value {
            Value::Object(_) => Ok(Value::Array(vec![value])),
            Value::Array(_) => Ok(value),
            other => Err(ToolArgumentError(format!(
                "'{field}' bir JSON dizisi olmalı, gelen tip: {}.",
                py_type_name(&other)
            ))),
        };
    }
    match value {
        Value::Object(_) => Ok(value),
        other => Err(ToolArgumentError(format!(
            "'{field}' bir JSON nesnesi olmalı, gelen tip: {}.",
            py_type_name(&other)
        ))),
    }
}

fn apply_aliases(tool_name: &str, arguments: &Map<String, Value>) -> Map<String, Value> {
    let mut result = arguments.clone();
    for key in ["*", tool_name] {
        for (tool, canonical, aliases) in PARAMETER_ALIASES {
            if *tool != key {
                continue;
            }
            for alias in *aliases {
                if result.contains_key(*alias) && !result.contains_key(*canonical) {
                    if let Some(v) = result.remove(*alias) {
                        result.insert((*canonical).to_string(), v);
                    }
                } else {
                    result.remove(*alias);
                }
            }
        }
    }
    result
}

fn edit_alias(key: &str) -> &str {
    EDIT_ITEM_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, canonical)| *canonical)
        .unwrap_or(key)
}

/// `edit_file.edits` listesini normalleştirir (takma adlar + tip denetimi).
pub fn normalize_edit_items(
    edits: Option<Value>,
) -> Result<Vec<Map<String, Value>>, ToolArgumentError> {
    let edits = match edits {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(m)) => vec![Value::Object(m)],
        Some(Value::Array(items)) => items,
        Some(other) => {
            return Err(ToolArgumentError(format!(
                "'edits' bir JSON dizisi olmalı, gelen tip: {}.",
                py_type_name(&other)
            )))
        }
    };
    let mut normalized = Vec::new();
    for (index, mut item) in edits.into_iter().enumerate() {
        if let Value::String(s) = &item {
            if s.trim().is_empty() {
                continue;
            }
            item = load_json_string(s, &format!("edits[{index}]"))?;
        }
        let Value::Object(map) = item else {
            return Err(ToolArgumentError(format!(
                "edits[{index}] bir JSON nesnesi olmalı (hedef: target_snippet/replacement_snippet), gelen tip: {}.",
                py_type_name(&item)
            )));
        };
        let canonical_present: HashSet<&str> = map
            .keys()
            .map(String::as_str)
            .filter(|k| !EDIT_ITEM_ALIASES.iter().any(|(a, _)| a == k))
            .collect();
        let mut edit = Map::new();
        for (key, value) in &map {
            let canonical = edit_alias(key);
            if canonical != key && canonical_present.contains(canonical) {
                continue;
            }
            if edit.contains_key(canonical) && canonical != "all" {
                continue;
            }
            edit.insert(canonical.to_string(), value.clone());
        }
        let target = edit
            .get("target_snippet")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new()));
        let replacement = edit
            .get("replacement_snippet")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new()));
        if !target.is_string() || !replacement.is_string() {
            return Err(ToolArgumentError(format!(
                "edits[{index}] içinde target_snippet ve replacement_snippet metin olmalı."
            )));
        }
        edit.insert("target_snippet".to_string(), target);
        edit.insert("replacement_snippet".to_string(), replacement);
        let all = edit.get("all").cloned().unwrap_or(Value::Bool(false));
        let all = coerce_value(all, "boolean", &format!("edits[{index}].all"))?;
        edit.insert(
            "all".to_string(),
            if all.is_null() {
                Value::Bool(false)
            } else {
                all
            },
        );
        normalized.push(edit);
    }
    if normalized.is_empty() {
        return Err(ToolArgumentError(
            "'edits' boş; en az bir {target_snippet, replacement_snippet} nesnesi gerekir."
                .to_string(),
        ));
    }
    Ok(normalized)
}

/// Argümanları bildirilen şema tiplerine dönüştürür.
pub fn normalize_tool_arguments(
    tool_name: &str,
    arguments: Value,
    type_map: Option<&TypeMap>,
    required_map: Option<&RequiredMap>,
) -> Result<Map<String, Value>, ToolArgumentError> {
    let mut arguments = arguments;
    if let Value::String(s) = &arguments {
        arguments = if s.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            let field = if tool_name.is_empty() {
                "arguments"
            } else {
                tool_name
            };
            load_json_string(s, field)?
        };
    }
    let arguments = match arguments {
        Value::Null => Map::new(),
        Value::Object(m) => m,
        Value::Array(mut items) if items.len() == 1 && items[0].is_object() => {
            match items.remove(0) {
                Value::Object(m) => m,
                _ => Map::new(),
            }
        }
        other => {
            return Err(ToolArgumentError(format!(
                "'{tool_name}' araç argümanları bir JSON nesnesi olmalı, gelen tip: {}.",
                py_type_name(&other)
            )))
        }
    };

    let mut result = apply_aliases(tool_name, &arguments);
    let empty = HashMap::new();
    let declared = type_map.and_then(|m| m.get(tool_name)).unwrap_or(&empty);
    let keys: Vec<String> = result.keys().cloned().collect();
    for field in keys {
        let Some(expected) = declared.get(&field) else {
            continue;
        };
        let current = result.remove(&field).unwrap_or(Value::Null);
        let coerced = coerce_value(current, expected, &field)?;
        result.insert(field, coerced);
    }

    if tool_name == "edit_file" && result.contains_key("edits") {
        let path_missing = !result.get("path").map(is_truthy).unwrap_or(false);
        if path_missing {
            if let Some(Value::Array(items)) = result.get("edits") {
                if !items.is_empty() {
                    let all_have_path = items.iter().all(|i| {
                        i.as_object()
                            .and_then(|o| o.get("path"))
                            .map(is_truthy)
                            .unwrap_or(false)
                    });
                    let paths: HashSet<String> = items
                        .iter()
                        .filter_map(|i| i.as_object()?.get("path"))
                        .filter(|p| is_truthy(p))
                        .map(Value::to_string)
                        .collect();
                    if paths.len() == 1 && all_have_path {
                        let first = items[0].as_object().and_then(|o| o.get("path")).cloned();
                        let stripped: Vec<Value> = items
                            .iter()
                            .map(|i| match i {
                                Value::Object(o) => Value::Object(
                                    o.iter()
                                        .filter(|(k, _)| k.as_str() != "path")
                                        .map(|(k, v)| (k.clone(), v.clone()))
                                        .collect(),
                                ),
                                other => other.clone(),
                            })
                            .collect();
                        if let Some(p) = first {
                            result.insert("path".to_string(), p);
                        }
                        result.insert("edits".to_string(), Value::Array(stripped));
                    }
                }
            }
        }
        let items = normalize_edit_items(result.remove("edits"))?;
        result.insert(
            "edits".to_string(),
            Value::Array(items.into_iter().map(Value::Object).collect()),
        );
    }

    let missing: Vec<&str> = required_map
        .and_then(|m| m.get(tool_name))
        .map(|req| {
            req.iter()
                .map(String::as_str)
                .filter(|f| result.get(*f).map(Value::is_null).unwrap_or(true))
                .collect()
        })
        .unwrap_or_default();
    if !missing.is_empty() {
        return Err(ToolArgumentError(format!(
            "'{tool_name}' için zorunlu parametreler eksik: {}.",
            missing.join(", ")
        )));
    }
    Ok(result)
}

/// Python doğruluk değeri (`bool(value)`).
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
