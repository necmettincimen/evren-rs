//! Çıktı dili (Python `language.py`): proje hafızası profilindeki `language`.

use std::path::Path;

use crate::memory::{load_memory, profile, set_profile, Scope};

const ALIASES: &[(&str, &str)] = &[
    ("tr", "Türkçe"),
    ("turkish", "Türkçe"),
    ("türkçe", "Türkçe"),
    ("turkce", "Türkçe"),
    ("en", "English"),
    ("english", "English"),
    ("ingilizce", "English"),
    ("de", "Deutsch"),
    ("german", "Deutsch"),
    ("almanca", "Deutsch"),
    ("fr", "Français"),
    ("french", "Français"),
    ("fransızca", "Français"),
    ("es", "Español"),
    ("spanish", "Español"),
    ("ispanyolca", "Español"),
];

/// Takma ad/kodu kanonik dil adına çevirir (bilinmeyen ad olduğu gibi döner).
pub fn resolve_language(name: &str) -> String {
    let key = name.trim().to_lowercase();
    if key.is_empty() {
        return String::new();
    }
    ALIASES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| (*v).to_string())
        .unwrap_or_else(|| name.trim().to_string())
}

/// Kayıtlı çıktı dili; yoksa "".
pub fn get_language(workspace_root: &Path) -> String {
    profile(&load_memory(workspace_root, Scope::Project))
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Çıktı dilini kaydeder, kanonik adı döndürür.
pub fn set_language(name: &str, workspace_root: &Path) -> Result<String, String> {
    let canonical = resolve_language(name);
    if canonical.is_empty() {
        return Err("Dil adı boş olamaz.".into());
    }
    set_profile("language", &canonical, workspace_root, Scope::Project)?;
    Ok(canonical)
}

/// Sistem istemi için dil bloğu; dil ayarlı değilse "".
pub fn language_context_block(workspace_root: &Path) -> String {
    let lang = get_language(workspace_root);
    if lang.is_empty() {
        return String::new();
    }
    format!(
        "DİL MODU: Tüm açıklama ve raporlarını {lang} dilinde yaz. \
         Kod, tanımlayıcılar ve teknik terimler İngilizce kalır; \
         yalnızca kullanıcıya dönük düzyazı seçilen dilde olur."
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn resolve_aliases() {
        assert_eq!(resolve_language("tr"), "Türkçe");
        assert_eq!(resolve_language(" EN "), "English");
        assert_eq!(resolve_language("Klingon"), "Klingon");
        assert_eq!(resolve_language("  "), "");
    }

    #[test]
    fn set_and_block() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(language_context_block(dir.path()), "");
        assert_eq!(set_language("de", dir.path()).unwrap(), "Deutsch");
        assert_eq!(get_language(dir.path()), "Deutsch");
        assert!(language_context_block(dir.path()).contains("Deutsch dilinde"));
        assert!(set_language("", dir.path()).is_err());
    }
}
