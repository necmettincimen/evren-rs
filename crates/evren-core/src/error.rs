//! Tek hata türü. Python `errors.py` sınıf hiyerarşisinin düz karşılığı;
//! `retryable()` / `failover()` bayrakları orada sınıf başına sabitti.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum EvrenError {
    /// 429, yeniden denenebilir.
    #[error("{message}")]
    RateLimit {
        message: String,
        retry_after: Option<f64>,
        failed_key: Option<String>,
    },
    /// 429 + kota işareti: anahtar günlük kotasını tüketti, başka anahtara geç.
    #[error("{message}")]
    QuotaExhausted {
        message: String,
        failed_key: Option<String>,
    },
    /// 401/402/403: başka anahtara geç.
    #[error("{message}")]
    Auth {
        message: String,
        status_code: Option<u16>,
        failed_key: Option<String>,
    },
    /// 403 + `terms_not_accepted`: kullanım şartları kabul edilmemiş.
    #[error("{message}")]
    TermsNotAccepted {
        message: String,
        failed_key: Option<String>,
    },
    /// 5xx, yeniden denenebilir.
    #[error("{message}")]
    Server {
        message: String,
        status_code: Option<u16>,
        failed_key: Option<String>,
    },
    /// Bağlantı / zaman aşımı, yeniden denenebilir.
    #[error("{message}")]
    Network {
        message: String,
        failed_key: Option<String>,
    },
    /// Diğer HTTP hataları.
    #[error("{message}")]
    Api {
        message: String,
        status_code: Option<u16>,
        failed_key: Option<String>,
    },
    /// Yapılandırma hatası (eksik anahtar, geçersiz tema vb.).
    #[error("{0}")]
    Config(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

impl EvrenError {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            EvrenError::RateLimit { .. } | EvrenError::Server { .. } | EvrenError::Network { .. }
        )
    }

    pub fn failover(&self) -> bool {
        matches!(
            self,
            EvrenError::QuotaExhausted { .. } | EvrenError::Auth { .. }
        )
    }

    pub fn status_code(&self) -> Option<u16> {
        match self {
            EvrenError::RateLimit { .. } | EvrenError::QuotaExhausted { .. } => Some(429),
            EvrenError::TermsNotAccepted { .. } => Some(403),
            EvrenError::Auth { status_code, .. }
            | EvrenError::Server { status_code, .. }
            | EvrenError::Api { status_code, .. } => *status_code,
            _ => None,
        }
    }

    pub fn retry_after(&self) -> Option<f64> {
        match self {
            EvrenError::RateLimit { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    pub fn failed_key(&self) -> Option<&str> {
        match self {
            EvrenError::RateLimit { failed_key, .. }
            | EvrenError::QuotaExhausted { failed_key, .. }
            | EvrenError::Auth { failed_key, .. }
            | EvrenError::TermsNotAccepted { failed_key, .. }
            | EvrenError::Server { failed_key, .. }
            | EvrenError::Network { failed_key, .. }
            | EvrenError::Api { failed_key, .. } => failed_key.as_deref(),
            _ => None,
        }
    }

    /// Python istisna sınıfının adı (`type(e).__name__` karşılığı).
    pub fn kind_name(&self) -> &'static str {
        match self {
            EvrenError::RateLimit { .. } => "RateLimitError",
            EvrenError::QuotaExhausted { .. } => "QuotaExhaustedError",
            EvrenError::Auth { .. } => "AuthError",
            EvrenError::TermsNotAccepted { .. } => "TermsNotAcceptedError",
            EvrenError::Server { .. } => "ServerError",
            EvrenError::Network { .. } => "NetworkError",
            EvrenError::Api { .. } => "APIError",
            EvrenError::Config(_) => "ConfigError",
            EvrenError::Io { .. } => "OSError",
            EvrenError::Json(_) => "JSONDecodeError",
            EvrenError::Other(_) => "Exception",
        }
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        EvrenError::Io {
            path: path.into(),
            source,
        }
    }
}
