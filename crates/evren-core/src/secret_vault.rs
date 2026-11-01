//! Gizli değer kasası (`secret_vault.py`): istekteki parola/jeton benzeri
//! değerleri `{{EVREN_SECRET_n}}` yer tutucularına alır, çıktıda maskeler,
//! araç çağrısında geri açar.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

fn placeholder(n: usize) -> String {
    format!("{{{{EVREN_SECRET_{n}}}}}")
}

fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile_static(r"\{\{EVREN_SECRET_(\d+)\}\}"))
}

/// Sabit bir deseni derler; derlenemezse hiçbir şeyle eşleşmeyen desen döner
/// (sabit desenler testte doğrulanır, panik yok).
pub(crate) fn compile_static(pat: &str) -> Regex {
    match Regex::new(pat) {
        Ok(re) => re,
        Err(_) => Regex::new(r"[^\s\S]")
            .unwrap_or_else(|_| Regex::new("a").unwrap_or_else(|_| std::process::abort())),
    }
}

/// Yakalama desenleri; her birinde 1. grup gizli değerdir.
fn patterns() -> &'static [Regex] {
    static PATS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATS.get_or_init(|| {
        [
            r"(?i)\b(?:bearer|basic|digest)\s+([A-Za-z0-9._~+/=-]{8,})",
            r#"(?i)\b(?:password|passwd|pwd|secret|token|api[_-]?key|private[_-]?token)["']?\s*[:=]\s*["']?([^\s"',;&]{8,})"#,
            r"(?i)\b[a-z][a-z0-9+.-]*://[^/\s:@]+:([^@\s/]{4,})@",
            r"\b(evren_llm_[A-Za-z0-9_-]{8,})",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    })
}

#[derive(Debug, Default, Clone)]
pub struct SecretVault {
    /// yer tutucu → gerçek değer
    secrets: HashMap<String, String>,
    /// gerçek değer → yer tutucu
    reverse: HashMap<String, String>,
}

impl SecretVault {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    fn register(&mut self, value: &str) -> String {
        if let Some(p) = self.reverse.get(value) {
            return p.clone();
        }
        let p = placeholder(self.secrets.len() + 1);
        self.secrets.insert(p.clone(), value.to_owned());
        self.reverse.insert(value.to_owned(), p.clone());
        p
    }

    /// Metindeki gizli değerleri yer tutucuyla değiştirir; aynı değer aynı
    /// numarayı alır. Sonunda bilinen tüm değerler ayrıca maskelenir.
    pub fn capture(&mut self, text: &str) -> String {
        let mut out = text.to_owned();
        for re in patterns() {
            let mut result = String::with_capacity(out.len());
            let mut last = 0;
            for caps in re.captures_iter(&out) {
                let Some(g) = caps.get(1) else { continue };
                let value = g.as_str();
                if placeholder_re().is_match(value) {
                    continue;
                }
                let ph = self.register(value);
                result.push_str(&out[last..g.start()]);
                result.push_str(&ph);
                last = g.end();
            }
            result.push_str(&out[last..]);
            out = result;
        }
        self.mask(&out)
    }

    /// Bilinen değerleri (uzundan kısaya) yer tutucuyla değiştirir.
    pub fn mask(&self, text: &str) -> String {
        if self.secrets.is_empty() {
            return text.to_owned();
        }
        let mut pairs: Vec<(&String, &String)> = self.reverse.iter().collect();
        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
        let mut out = text.to_owned();
        for (value, ph) in pairs {
            if !value.is_empty() {
                out = out.replace(value.as_str(), ph);
            }
        }
        out
    }

    /// Yer tutucuları gerçek değerlere açar; bilinmeyen numara olduğu gibi kalır.
    pub fn expand(&self, text: &str) -> String {
        if self.secrets.is_empty() {
            return text.to_owned();
        }
        placeholder_re()
            .replace_all(text, |caps: &regex::Captures| {
                let whole = &caps[0];
                self.secrets
                    .get(whole)
                    .cloned()
                    .unwrap_or_else(|| whole.to_owned())
            })
            .into_owned()
    }

    /// Alt süreçler için `EVREN_SECRET_n` → değer eşlemi.
    pub fn env(&self) -> HashMap<String, String> {
        self.secrets
            .iter()
            .map(|(ph, v)| {
                (
                    ph.trim_matches(|c| c == '{' || c == '}').to_owned(),
                    v.clone(),
                )
            })
            .collect()
    }

    /// Yer tutucu adları, numara sırasıyla.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<(usize, String)> = self
            .secrets
            .keys()
            .filter_map(|ph| {
                placeholder_re()
                    .captures(ph)
                    .and_then(|c| c.get(1))
                    .and_then(|m| m.as_str().parse::<usize>().ok())
                    .map(|n| (n, ph.clone()))
            })
            .collect();
        names.sort();
        names.into_iter().map(|(_, p)| p).collect()
    }

    /// Sistem istemine eklenen Türkçe not; kasa boşsa boş dize.
    pub fn usage_note(&self) -> String {
        if self.secrets.is_empty() {
            return String::new();
        }
        let names = self.names().join(", ");
        format!(
            "[EVREN GİZLİ DEĞERLER] Görevdeki gizli değerler {names} yer tutucularına alındı; \
             gerçek değeri göremezsin ve tahmin etme. run_command/ssh_run komutlarında yer \
             tutucuyu aynen yaz (örn. -H \"Authorization: Bearer {{{{EVREN_SECRET_1}}}}\"); CLI çalıştırırken \
             gerçek değeri koyar. Dosyaya (betik, yapılandırma) yer tutucu veya değer yazma; \
             betikte ortam değişkeninden oku: PowerShell $env:EVREN_SECRET_1, cmd %EVREN_SECRET_1%, \
             Python os.environ['EVREN_SECRET_1']. Bu değişkenler her komutta zaten tanımlıdır; \
             `set`/`export` ile yeniden atama."
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const TOKEN: &str = "abcDEF123456xyz";

    #[test]
    fn capture_mask_expand() {
        let mut v = SecretVault::new();
        let text = format!("curl -H 'Authorization: Bearer {TOKEN}' --data password=hunter2xyz");
        let captured = v.capture(&text);
        assert!(captured.contains("Bearer {{EVREN_SECRET_1}}"), "{captured}");
        assert!(
            captured.contains("password={{EVREN_SECRET_2}}"),
            "{captured}"
        );
        assert!(!captured.contains(TOKEN));
        assert_eq!(v.env()["EVREN_SECRET_1"], TOKEN);
        assert_eq!(v.expand(&captured), text);
        assert_eq!(
            v.mask(&format!("çıktı: {TOKEN}")),
            "çıktı: {{EVREN_SECRET_1}}"
        );
        // Aynı değer aynı numara.
        let again = v.capture(&format!("Bearer {TOKEN}"));
        assert_eq!(again, "Bearer {{EVREN_SECRET_1}}");
        assert_eq!(v.len(), 2);
        assert!(v.usage_note().starts_with("[EVREN GİZLİ DEĞERLER]"));
        assert!(v
            .usage_note()
            .contains("{{EVREN_SECRET_1}}, {{EVREN_SECRET_2}}"));
    }

    #[test]
    fn url_password_and_evren_key() {
        let mut v = SecretVault::new();
        let out = v.capture("psql postgres://user:p4ssw0rd@db/x ve evren_llm_ABCDEFGH12");
        assert_eq!(
            out,
            "psql postgres://user:{{EVREN_SECRET_1}}@db/x ve {{EVREN_SECRET_2}}"
        );
    }

    #[test]
    fn empty_vault_is_identity() {
        let v = SecretVault::new();
        assert_eq!(v.mask("x"), "x");
        assert_eq!(v.expand("{{EVREN_SECRET_9}}"), "{{EVREN_SECRET_9}}");
        assert_eq!(v.usage_note(), "");
    }
}
