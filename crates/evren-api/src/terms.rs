//! Kullanım şartları (`terms.py`): durum sorgusu, metin, kabul ve
//! etkileşimli onay akışı. Arayüz çıktısı `TermsUi` üzerinden verilir.

use evren_core::{texts, EvrenError};
use serde_json::{json, Value};

use crate::client::ApiClient;
use crate::key_pool::mask_key;

const TERMS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Şartlar akışının çıktılarını alan arayüz (CLI, REPL, stdio).
pub trait TermsUi {
    fn info(&mut self, msg: &str);
    fn success(&mut self, msg: &str);
    fn warning(&mut self, msg: &str);
    fn error(&mut self, msg: &str);
    /// Önek olmadan ham satır (şart metni).
    fn raw(&mut self, msg: &str);
    /// Evet/hayır sorusu; varsayılan hayırdır.
    fn confirm(&mut self, msg: &str) -> bool;
}

#[derive(Debug, Clone, Copy)]
pub struct TermsOptions {
    pub interactive: bool,
    pub force: bool,
    /// Zaten kabul edilmişse başarı satırını yazma.
    pub silent_ok: bool,
}

impl Default for TermsOptions {
    fn default() -> Self {
        TermsOptions {
            interactive: true,
            force: false,
            silent_ok: false,
        }
    }
}

/// `_as_bool`: bool, sayı ya da `true/1/yes/evet` dizgesi.
pub fn as_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => matches!(
            s.trim().to_lowercase().as_str(),
            "true" | "1" | "yes" | "evet"
        ),
        _ => false,
    }
}

fn other(msg: String) -> EvrenError {
    EvrenError::Other(msg)
}

impl ApiClient {
    fn terms_key(&self, api_key: Option<&str>) -> Result<String, EvrenError> {
        match api_key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => Ok(k.to_owned()),
            None => self
                .current_key()
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| other(texts::TERMS_KEY_MISSING.to_owned())),
        }
    }

    async fn terms_call(
        &self,
        method: reqwest::Method,
        path: &str,
        api_key: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, EvrenError> {
        let key = self.terms_key(api_key)?;
        let mut rb = self.request(method, path, &key).timeout(TERMS_TIMEOUT);
        if let Some(b) = &body {
            rb = rb.json(b);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| other(texts::terms_connect_failed(&e)))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status == 401 {
            return Err(other(texts::TERMS_KEY_INVALID.to_owned()));
        }
        if !(200..300).contains(&status) {
            return Err(other(texts::terms_status_failed(status, &text.trim())));
        }
        serde_json::from_str(&text).map_err(|e| other(texts::terms_status_failed(status, &e)))
    }

    /// GET /terms/status → ham JSON (`accepted`, `current_version`, ...).
    pub async fn get_terms_status(&self, api_key: Option<&str>) -> Result<Value, EvrenError> {
        self.terms_call(reqwest::Method::GET, "/terms/status", api_key, None)
            .await
    }

    /// GET /terms → `content` metni (ya da ham JSON).
    pub async fn get_terms_text(&self, api_key: Option<&str>) -> Result<String, EvrenError> {
        let v = self
            .terms_call(reqwest::Method::GET, "/terms", api_key, None)
            .await
            .map_err(|e| other(texts::terms_text_failed(&e)))?;
        Ok(match v.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => match &v {
                Value::String(s) => s.clone(),
                _ => String::new(),
            },
            Some(o) => o.to_string(),
        })
    }

    /// POST /terms/accept `{"version": n}`.
    pub async fn accept_terms(
        &self,
        version: i64,
        api_key: Option<&str>,
    ) -> Result<Value, EvrenError> {
        self.terms_call(
            reqwest::Method::POST,
            "/terms/accept",
            api_key,
            Some(json!({"version": version})),
        )
        .await
        .map_err(|e| other(texts::terms_accept_failed(&e)))
    }
}

fn version_of(status: &Value) -> i64 {
    status
        .get("current_version")
        .or_else(|| status.get("version"))
        .and_then(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        })
        .unwrap_or(1)
}

/// `ensure_terms_accepted`: durumu kontrol eder, gerekirse metni gösterip
/// onay ister ve kabul eder. Dönüş: şartlar (artık) kabul edilmiş mi.
pub async fn ensure_terms_accepted(
    client: &ApiClient,
    api_key: Option<&str>,
    opts: TermsOptions,
    ui: &mut dyn TermsUi,
) -> bool {
    let status = match client.get_terms_status(api_key).await {
        Ok(s) => s,
        Err(e) => {
            ui.warning(&texts::terms_check_failed(&e));
            return false;
        }
    };
    let accepted = status.get("accepted").is_some_and(as_bool);
    let version = version_of(&status);
    if accepted && !opts.force {
        if !opts.silent_ok {
            ui.success(&texts::terms_already_accepted(version));
        }
        return true;
    }
    if !accepted {
        ui.warning(&texts::terms_not_accepted_yet(version));
    }
    if !opts.interactive {
        ui.error(texts::TERMS_NON_INTERACTIVE);
        return false;
    }
    match client.get_terms_text(api_key).await {
        Ok(content) => {
            let sep = "=".repeat(60);
            ui.raw(&format!("\n{sep}"));
            ui.raw(&texts::terms_title(version));
            ui.raw(&sep);
            if content.is_empty() {
                ui.raw(texts::TERMS_EMPTY_TEXT);
            } else {
                let shown: String = content.chars().take(1500).collect();
                if content.chars().count() > 1500 {
                    ui.raw(&format!("{shown}\n... [Devamı var]"));
                } else {
                    ui.raw(&shown);
                }
            }
            ui.raw(&format!("{sep}\n"));
        }
        Err(e) => ui.warning(&texts::terms_text_unreadable(&e)),
    }
    if !ui.confirm(&texts::terms_confirm(version)) {
        ui.warning(texts::TERMS_REJECTED_HINT);
        return false;
    }
    match client.accept_terms(version, api_key).await {
        Ok(_) => {
            ui.success(&texts::terms_accepted_ok(version));
            true
        }
        Err(e) => {
            ui.error(&texts::terms_accept_op_failed(&e));
            false
        }
    }
}

/// `ensure_terms_for_keys`: her anahtar için ayrı kontrol; hepsi kabulse true.
pub async fn ensure_terms_for_keys(
    client: &ApiClient,
    keys: &[String],
    interactive: bool,
    force: bool,
    ui: &mut dyn TermsUi,
) -> bool {
    if keys.is_empty() {
        ui.warning(texts::TERMS_NO_KEYS);
        return false;
    }
    let mut all_ok = true;
    for key in keys {
        ui.info(&texts::terms_checking_key(&mask_key(key)));
        let ok = ensure_terms_accepted(
            client,
            Some(key),
            TermsOptions {
                interactive,
                force,
                silent_ok: false,
            },
            ui,
        )
        .await;
        all_ok &= ok;
    }
    all_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_coercion() {
        assert!(as_bool(&json!(true)));
        assert!(as_bool(&json!(1)));
        assert!(as_bool(&json!("Evet")));
        assert!(as_bool(&json!(" yes ")));
        assert!(!as_bool(&json!("no")));
        assert!(!as_bool(&json!(0)));
        assert!(!as_bool(&Value::Null));
    }

    #[test]
    fn version_fallback() {
        assert_eq!(version_of(&json!({"current_version": 3})), 3);
        assert_eq!(version_of(&json!({"version": "2"})), 2);
        assert_eq!(version_of(&json!({})), 1);
    }
}
