//! HTTP yanıtlarını ve ağ hatalarını `EvrenError` türlerine eşler (`errors.py`).

use std::collections::HashMap;

use evren_core::{texts, EvrenError};

const QUOTA_MARKERS: [&str; 15] = [
    "insufficient_quota",
    "quota_exceeded",
    "quota exceeded",
    "daily quota",
    "daily_quota",
    "daily limit",
    "daily_limit",
    "exceeded your current quota",
    "out of quota",
    "no remaining tokens",
    "günlük kota",
    "günlük limit",
    "kota tükendi",
    "kota aşıldı",
    "kota doldu",
];

pub fn looks_like_quota_exhausted(text: &str) -> bool {
    let low = text.to_lowercase();
    QUOTA_MARKERS.iter().any(|m| low.contains(m))
}

/// `Retry-After` başlığını saniyeye çevirir (sayı değilse None).
pub fn retry_after_from(headers: &HashMap<String, String>) -> Option<f64> {
    headers
        .get("retry-after")
        .and_then(|v| v.trim().parse::<f64>().ok())
}

/// Durum kodu + gövdeden türlenmiş hata. Metin OpenAI SDK biçimini izler:
/// `Error code: {status} - {body}`.
pub fn from_status(
    status: u16,
    headers: &HashMap<String, String>,
    body: &str,
    failed_key: Option<&str>,
) -> EvrenError {
    let key = failed_key.map(str::to_owned);
    let body = body.trim();
    let message = if body.is_empty() {
        format!("Error code: {status}")
    } else {
        format!("Error code: {status} - {body}")
    };
    if message.contains("terms_not_accepted") {
        return EvrenError::TermsNotAccepted {
            message: texts::TERMS_DEFAULT.to_owned(),
            failed_key: key,
        };
    }
    let retry_after = retry_after_from(headers);
    match status {
        429 => {
            if looks_like_quota_exhausted(&message) {
                EvrenError::QuotaExhausted {
                    message,
                    failed_key: key,
                }
            } else {
                EvrenError::RateLimit {
                    message,
                    retry_after,
                    failed_key: key,
                }
            }
        }
        401..=403 => EvrenError::Auth {
            message,
            status_code: Some(status),
            failed_key: key,
        },
        500..=599 => EvrenError::Server {
            message,
            status_code: Some(status),
            failed_key: key,
        },
        _ => EvrenError::Api {
            message,
            status_code: Some(status),
            failed_key: key,
        },
    }
}

/// `reqwest` taşıma hatası (bağlantı, zaman aşımı, TLS) → ağ hatası;
/// durum hatası taşıyorsa koda göre eşlenir.
pub fn from_reqwest(e: &reqwest::Error, failed_key: Option<&str>) -> EvrenError {
    let key = failed_key.map(str::to_owned);
    if let Some(status) = e.status() {
        return from_status(status.as_u16(), &HashMap::new(), &e.to_string(), failed_key);
    }
    if e.is_timeout() || e.is_connect() || e.is_request() || e.is_body() || e.is_decode() {
        return EvrenError::Network {
            message: e.to_string(),
            failed_key: key,
        };
    }
    classify_text(&e.to_string(), failed_key)
}

/// Son çare dize sezgileri (`map_sdk_exception` kuyruğu).
pub fn classify_text(text: &str, failed_key: Option<&str>) -> EvrenError {
    let key = failed_key.map(str::to_owned);
    let message = text.to_owned();
    let low = text.to_lowercase();
    if low.contains("terms_not_accepted") {
        return EvrenError::TermsNotAccepted {
            message: texts::TERMS_DEFAULT.to_owned(),
            failed_key: key,
        };
    }
    if text.contains("429") {
        if looks_like_quota_exhausted(text) {
            return EvrenError::QuotaExhausted {
                message,
                failed_key: key,
            };
        }
        return EvrenError::RateLimit {
            message,
            retry_after: None,
            failed_key: key,
        };
    }
    if text.contains("503") || text.contains("502") || text.contains("500") {
        return EvrenError::Server {
            message,
            status_code: None,
            failed_key: key,
        };
    }
    if low.contains("connection error")
        || low.contains("connection reset")
        || low.contains("connection aborted")
        || low.contains("connection refused")
        || low.contains("timed out")
        || low.contains("read timeout")
        || low.contains("network is unreachable")
    {
        return EvrenError::Network {
            message,
            failed_key: key,
        };
    }
    EvrenError::Api {
        message,
        status_code: None,
        failed_key: key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn status_mapping() {
        let e = from_status(
            429,
            &h(&[("retry-after", "2.5")]),
            r#"{"error":"rate limited"}"#,
            Some("k"),
        );
        assert!(
            matches!(e, EvrenError::RateLimit { retry_after: Some(r), .. } if (r - 2.5).abs() < 1e-9)
        );
        assert!(e.retryable());
        assert_eq!(e.failed_key(), Some("k"));
        let q = from_status(429, &h(&[]), "Günlük kota tükendi", None);
        assert!(matches!(q, EvrenError::QuotaExhausted { .. }));
        assert!(q.failover() && !q.retryable());
        assert!(matches!(
            from_status(401, &h(&[]), "", None),
            EvrenError::Auth {
                status_code: Some(401),
                ..
            }
        ));
        assert!(matches!(
            from_status(503, &h(&[]), "x", None),
            EvrenError::Server { .. }
        ));
        assert!(matches!(
            from_status(400, &h(&[]), "x", None),
            EvrenError::Api {
                status_code: Some(400),
                ..
            }
        ));
        let t = from_status(403, &h(&[]), r#"{"code":"terms_not_accepted"}"#, None);
        assert!(matches!(t, EvrenError::TermsNotAccepted { .. }));
        assert!(!t.failover());
        assert_eq!(
            from_status(400, &h(&[]), "", None).to_string(),
            "Error code: 400"
        );
    }

    #[test]
    fn text_heuristics() {
        assert!(matches!(
            classify_text("Connection error.", None),
            EvrenError::Network { .. }
        ));
        assert!(matches!(
            classify_text("HTTP 503", None),
            EvrenError::Server { .. }
        ));
        assert!(matches!(
            classify_text("429 quota_exceeded", None),
            EvrenError::QuotaExhausted { .. }
        ));
        assert!(matches!(
            classify_text("bilinmeyen", None),
            EvrenError::Api { .. }
        ));
    }
}
