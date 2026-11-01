//! `EvrenClient` eşdeğeri: sohbet tamamlama (akışsız ve SSE akışlı),
//! model listesi, kota, düşük kotalı anahtardan en iyi anahtara geçiş.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use evren_core::config::{Config, SslVerify};
use evren_core::{models, texts, EvrenError};
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::errors;
use crate::key_pool::{ApiKeyPool, StateCounts};
use crate::quota::normalize_quota;
use crate::sse::{SseParser, StreamAssembler, StreamDelta};

/// `list_models` başarısız olursa dönen sabit liste (Python ile aynı).
pub const FALLBACK_MODELS: [&str; 4] = [
    "deepseek-v4.1-flash",
    "glm-5.3",
    "qwen3.8-flash-next",
    "auto",
];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const SHORT_TIMEOUT: Duration = Duration::from_secs(20);
/// Akışta iki parça arasındaki boşta kalma sınırı (`STREAM_TIMEOUT`).
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// `thinking: false` gönderildiğinde daha kısa boşta kalma sınırı.
const STREAM_IDLE_TIMEOUT_NO_THINK: Duration = Duration::from_secs(90);
/// Akış isteğinin toplam üst sınırı (istemci düzeyindeki 120 sn'yi geçersiz kılar).
const STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(24 * 3600);
/// Geçici (yeniden denenebilir) hatalarda akış öncesi deneme sayısı.
pub const TRANSIENT_RETRIES: u32 = 2;

/// `_transient_backoff`: Retry-After varsa o, yoksa `tries * 2` saniye.
pub fn transient_backoff(err: &EvrenError, tries: u32) -> f64 {
    match err.retry_after() {
        Some(ra) if ra > 0.0 => ra,
        _ => f64::from(tries) * 2.0,
    }
}

/// `_can_retry_transient`: akış başlamadıysa, deneme hakkı varsa ve hata
/// yeniden denenebilir türdeyse.
pub fn can_retry_transient(err: &EvrenError, streamed_any: bool, tries: u32) -> bool {
    !streamed_any && tries < TRANSIENT_RETRIES && err.retryable()
}

/// `chat_complete` parametreleri.
#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub messages: Vec<Value>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub tools: Option<Value>,
    pub tool_choice: Option<Value>,
    /// 0 verilirse 3 sayılır.
    pub max_retries: u32,
    /// `Some(false)` → destekleyen modellerde `thinking: false` gönderilir.
    pub thinking: Option<bool>,
}

impl ChatRequest {
    pub fn new(messages: Vec<Value>) -> Self {
        ChatRequest {
            messages,
            max_retries: 3,
            ..Default::default()
        }
    }

    pub fn simple(system: &str, user: &str) -> Self {
        Self::new(vec![
            json!({"role": "system", "content": system}),
            json!({"role": "user", "content": user}),
        ])
    }
}

/// Ham `chat/completions` yanıtı ve sık kullanılan alanlara erişim.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub raw: Value,
}

impl ChatResponse {
    fn message(&self) -> Option<&Value> {
        self.raw.get("choices")?.get(0)?.get("message")
    }

    pub fn content(&self) -> Option<&str> {
        self.message()?.get("content")?.as_str()
    }

    /// `choice.reasoning or choice.reasoning_content`
    pub fn reasoning(&self) -> Option<&str> {
        let m = self.message()?;
        m.get("reasoning")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                m.get("reasoning_content")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
    }

    pub fn tool_calls(&self) -> Option<&Vec<Value>> {
        self.message()?.get("tool_calls")?.as_array()
    }

    pub fn finish_reason(&self) -> Option<&str> {
        self.raw
            .get("choices")?
            .get(0)?
            .get("finish_reason")?
            .as_str()
    }

    pub fn usage(&self) -> Option<&Map<String, Value>> {
        self.raw.get("usage")?.as_object()
    }
}

#[derive(Debug)]
pub struct ApiClient {
    http: reqwest::Client,
    base_url: String,
    pool: Mutex<ApiKeyPool>,
    default_model: String,
    default_max_tokens: u32,
    default_temperature: f64,
    reasoning_effort: Mutex<Option<String>>,
    thinking_control: AtomicBool,
    last_remaining_tokens: Mutex<Option<i64>>,
    key_min_remaining: i64,
}

/// `select_best_key` sonucu: seçilen anahtar ve normalize kota satırı.
#[derive(Debug, Clone)]
pub struct BestKey {
    pub key: String,
    pub remaining: i64,
    pub quota: Map<String, Value>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn header_map(h: &reqwest::header::HeaderMap) -> HashMap<String, String> {
    h.iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_lowercase(), s.to_owned()))
        })
        .collect()
}

fn build_http(config: &Config) -> Result<reqwest::Client, EvrenError> {
    let mut b = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(READ_TIMEOUT)
        .user_agent(format!("evren-cli/{}", evren_core::VERSION));
    match config.ssl_verify() {
        SslVerify::Flag(false) => b = b.danger_accept_invalid_certs(true),
        SslVerify::Flag(true) => {}
        SslVerify::CaBundle(path) => {
            let pem = std::fs::read(&path).map_err(|e| EvrenError::io(&path, e))?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|e| EvrenError::Config(format!("EVREN_CA_BUNDLE okunamadı: {e}")))?;
            for c in certs {
                b = b.add_root_certificate(c);
            }
        }
    }
    b.build()
        .map_err(|e| EvrenError::Config(format!("HTTP istemcisi kurulamadı: {e}")))
}

/// Sistem mesajlarını tek bir baş mesajda toplar (`_normalize_messages`).
pub fn normalize_messages(messages: &[Value]) -> Vec<Value> {
    let mut system_parts: Vec<String> = Vec::new();
    let mut rest: Vec<Value> = Vec::new();
    for m in messages {
        let is_system = m.get("role").and_then(Value::as_str) == Some("system");
        if is_system {
            let text = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            if !text.trim().is_empty() {
                system_parts.push(text);
            }
        } else {
            rest.push(m.clone());
        }
    }
    let mut out = Vec::with_capacity(rest.len() + 1);
    if !system_parts.is_empty() {
        out.push(json!({"role": "system", "content": system_parts.join("\n\n")}));
    }
    out.extend(rest);
    out
}

impl ApiClient {
    pub fn new(config: &Config) -> Result<Self, EvrenError> {
        let keys = config.api_keys();
        if keys.is_empty() {
            return Err(EvrenError::Config(texts::API_KEY_MISSING.to_owned()));
        }
        Ok(ApiClient {
            http: build_http(config)?,
            base_url: config.base_url().trim_end_matches('/').to_owned(),
            pool: Mutex::new(ApiKeyPool::new(keys)),
            default_model: config.default_model(),
            default_max_tokens: config.default_max_tokens(),
            default_temperature: config.default_temperature(),
            reasoning_effort: Mutex::new(config.reasoning_effort()),
            thinking_control: AtomicBool::new(true),
            last_remaining_tokens: Mutex::new(None),
            key_min_remaining: config.key_min_remaining(),
        })
    }

    pub fn key_min_remaining(&self) -> i64 {
        self.key_min_remaining
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    pub fn current_key(&self) -> Option<String> {
        lock(&self.pool).current_key().map(str::to_owned)
    }

    pub fn key_states(&self) -> StateCounts {
        lock(&self.pool).state_counts()
    }

    pub fn last_remaining_tokens(&self) -> Option<i64> {
        *lock(&self.last_remaining_tokens)
    }

    pub fn with_pool<R>(&self, f: impl FnOnce(&mut ApiKeyPool) -> R) -> R {
        f(&mut lock(&self.pool))
    }

    pub(crate) fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        key: &str,
    ) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{}", self.base_url, path))
            .header("X-API-Key", key)
            .header("Authorization", format!("Bearer {key}"))
            .header("Content-Type", "application/json")
    }

    /// `_acquire_client`: hazır anahtar yoksa havuz özetiyle RateLimit hatası.
    fn acquire_key(&self) -> Result<String, EvrenError> {
        let mut pool = lock(&self.pool);
        if let Some(k) = pool.acquire() {
            return Ok(k);
        }
        let c = pool.state_counts();
        let wait = pool.next_available_secs();
        let wait_display: i64 = if wait > 0.0 {
            (wait.ceil() as i64).max(1)
        } else {
            0
        };
        Err(EvrenError::RateLimit {
            message: format!(
                "Tüm API anahtarları şu an kullanılamıyor (hazır: {}, soğumada: {}, devre dışı: {}). En erken {} sn sonra tekrar deneyin.",
                c.ready, c.cooling, c.disabled, wait_display
            ),
            retry_after: if wait > 0.0 { Some(wait) } else { None },
            failed_key: None,
        })
    }

    /// `_maybe_switch_low_quota_key`: geçerli anahtarın son görülen günlük
    /// kotası tabanın altındaysa (ve ≥2 anahtar varsa) en iyi anahtara geçer.
    /// Okuma tek seferliktir; geçiş başarısız olsa da sessizce devam edilir.
    async fn maybe_switch_low_quota_key(&self) {
        let low = {
            let mut pool = lock(&self.pool);
            if pool.len() < 2 {
                return;
            }
            let Some(key) = pool.current_key().map(str::to_owned) else {
                return;
            };
            match pool.forget_daily_remaining(&key) {
                Some(r) => r < self.key_min_remaining,
                None => false,
            }
        };
        if low {
            let _ = self.select_best_key(None).await;
        }
    }

    /// Sohbet yolları: önce düşük kota geçişi, sonra anahtar edinme.
    async fn acquire_key_switching(&self) -> Result<String, EvrenError> {
        self.maybe_switch_low_quota_key().await;
        self.acquire_key()
    }

    fn remaining_header(headers: &HashMap<String, String>) -> Option<i64> {
        let raw = headers.get("x-evren-daily-remaining-tokens")?.trim();
        raw.parse::<i64>()
            .ok()
            .or_else(|| raw.parse::<f64>().ok().map(|f| f as i64))
    }

    fn note_remaining_header(&self, headers: &HashMap<String, String>, key: &str) {
        if let Some(n) = Self::remaining_header(headers) {
            if self.current_key().as_deref() == Some(key) {
                *lock(&self.last_remaining_tokens) = Some(n);
            }
        }
    }

    /// Sohbet yanıtı başlıkları: `last_remaining_tokens` + havuz kaydı
    /// (Python'da yalnız sohbet istemcisindeki yanıt kancası kaydeder).
    fn note_chat_headers(&self, headers: &HashMap<String, String>, key: &str) {
        self.note_remaining_header(headers, key);
        if let Some(n) = Self::remaining_header(headers) {
            lock(&self.pool).record_daily_remaining(key, n);
        }
    }

    /// `select_best_key`: tüm anahtarların kotasını eşzamanlı çeker, en çok
    /// kalanı olanı sabitler; tabanın altındaki diğerlerini `quota_low` ile
    /// devre dışı bırakır. Hiçbir anahtar sayısal kota döndürmezse hata.
    pub async fn select_best_key(&self, min_remaining: Option<i64>) -> Result<BestKey, EvrenError> {
        let floor = min_remaining.unwrap_or(self.key_min_remaining);
        let keys = lock(&self.pool).key_values();
        let quotas =
            futures_util::future::join_all(keys.iter().map(|k| self.get_quota(Some(k.as_str()))))
                .await;
        let mut rows: Vec<(String, i64, Map<String, Value>)> = keys
            .into_iter()
            .zip(quotas)
            .filter_map(|(k, q)| {
                let remaining = q.get("remaining_daily_tokens").and_then(Value::as_i64)?;
                Some((k, remaining, q))
            })
            .collect();
        if rows.is_empty() {
            return Err(EvrenError::Other(texts::QUOTA_UNKNOWN.to_owned()));
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        let mut rows = rows.into_iter();
        let Some((best, remaining, quota)) = rows.next() else {
            return Err(EvrenError::Other(texts::QUOTA_UNKNOWN.to_owned()));
        };
        {
            let mut pool = lock(&self.pool);
            for (k, r, _) in rows {
                if r < floor {
                    pool.report_disabled(&k, "quota_low");
                }
            }
            pool.pin(&best);
        }
        *lock(&self.last_remaining_tokens) = Some(remaining);
        Ok(BestKey {
            key: best,
            remaining,
            quota,
        })
    }

    /// GET /models → kimlik listesi; her türlü hatada sabit yedek liste.
    pub async fn list_models(&self) -> Vec<String> {
        let fallback = || {
            FALLBACK_MODELS
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
        };
        let Some(key) = self.current_key().or_else(|| self.acquire_key().ok()) else {
            return fallback();
        };
        let resp = match self
            .request(reqwest::Method::GET, "/models", &key)
            .timeout(SHORT_TIMEOUT)
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return fallback(),
        };
        if !resp.status().is_success() {
            return fallback();
        }
        let data: Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return fallback(),
        };
        let items = match &data {
            Value::Object(o) => o
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            Value::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        let ids: Vec<String> = items
            .iter()
            .filter_map(|m| match m {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("id").and_then(Value::as_str).map(str::to_owned),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            fallback()
        } else {
            ids
        }
    }

    /// GET /quota → normalize edilmiş sözlük; hatada `error` alanı ile.
    pub async fn get_quota(&self, api_key: Option<&str>) -> Map<String, Value> {
        let key = match api_key {
            Some(k) => k.to_owned(),
            None => match self
                .current_key()
                .ok_or(())
                .or_else(|()| self.acquire_key())
            {
                Ok(k) => k,
                Err(e) => return self.quota_error(&e.to_string(), None),
            },
        };
        let is_current = self.current_key().as_deref() == Some(key.as_str());
        let resp = match self
            .request(reqwest::Method::GET, "/quota", &key)
            .timeout(SHORT_TIMEOUT)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return self.quota_error(&e.to_string(), Some(is_current)),
        };
        let headers = header_map(resp.headers());
        self.note_remaining_header(&headers, &key);
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let e = errors::from_status(status.as_u16(), &headers, &body, Some(&key));
            return self.quota_error(&e.to_string(), Some(is_current));
        }
        let raw: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => return self.quota_error(&e.to_string(), Some(is_current)),
        };
        let data = if raw.is_object() {
            raw
        } else {
            json!({"raw": raw})
        };
        normalize_quota(&data, &headers)
    }

    fn quota_error(&self, message: &str, is_current: Option<bool>) -> Map<String, Value> {
        let remaining = match (is_current, self.last_remaining_tokens()) {
            (Some(true), Some(n)) => Value::from(n),
            (Some(true), None) => Value::Null,
            _ => Value::from(texts::QUOTA_UNKNOWN),
        };
        normalize_quota(
            &json!({"error": message, "remaining_daily_tokens": remaining}),
            &HashMap::new(),
        )
    }

    fn apply_reasoning_controls(
        &self,
        body: &mut Map<String, Value>,
        model: &str,
        thinking: Option<bool>,
    ) {
        if thinking == Some(false)
            && self.thinking_control.load(Ordering::Relaxed)
            && models::thinking_off_supported(model)
        {
            body.insert("thinking".into(), Value::Bool(false));
            return;
        }
        if let Some(effort) = lock(&self.reasoning_effort).as_deref() {
            if matches!(model, "deepseek-v4.1-flash" | "glm-5.3") {
                body.insert("reasoning_effort".into(), Value::from(effort));
            }
        }
    }

    /// `_drop_rejected_param`: sunucu bir parametreyi reddettiyse kaldır ve
    /// yeniden dene (true → tekrar).
    fn drop_rejected_param(&self, body: &mut Map<String, Value>, message: &str) -> bool {
        if message.contains("reasoning_effort") && body.contains_key("reasoning_effort") {
            body.remove("reasoning_effort");
            *lock(&self.reasoning_effort) = None;
            return true;
        }
        if message.contains("thinking") && body.contains_key("thinking") {
            body.remove("thinking");
            self.thinking_control.store(false, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Akışsız ve akışlı yolların ortak istek gövdesi.
    fn build_body(&self, req: &ChatRequest) -> Map<String, Value> {
        let model = req
            .model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| self.default_model.clone());
        let tokens =
            models::floor_output_tokens(&model, req.max_tokens.unwrap_or(self.default_max_tokens));
        let mut body = Map::new();
        body.insert("model".into(), Value::from(model.clone()));
        body.insert(
            "messages".into(),
            Value::Array(normalize_messages(&req.messages)),
        );
        body.insert("max_tokens".into(), Value::from(tokens));
        body.insert(
            "temperature".into(),
            Value::from(req.temperature.unwrap_or(self.default_temperature)),
        );
        if let Some(tools) = &req.tools {
            body.insert("tools".into(), tools.clone());
            if let Some(tc) = &req.tool_choice {
                body.insert("tool_choice".into(), tc.clone());
            }
        }
        self.apply_reasoning_controls(&mut body, &model, req.thinking);
        body
    }

    /// Akışsız sohbet tamamlama; anahtar havuzu, soğuma ve yeniden deneme
    /// mantığı Python `chat_complete` ile aynıdır.
    pub async fn chat_complete(&self, req: ChatRequest) -> Result<ChatResponse, EvrenError> {
        let mut body = self.build_body(&req);

        let max_retries = if req.max_retries == 0 {
            3
        } else {
            req.max_retries
        };
        let mut attempt: u32 = 0;
        let mut last_error: Option<EvrenError> = None;

        while attempt < max_retries {
            let key = match self.acquire_key_switching().await {
                Ok(k) => k,
                Err(e) => {
                    let ra = e.retry_after().unwrap_or(0.0);
                    if attempt + 1 < max_retries && ra > 0.0 {
                        tokio::time::sleep(Duration::from_secs_f64(ra)).await;
                        lock(&self.pool).clear_all_cooldowns();
                        attempt += 1;
                        continue;
                    }
                    return Err(e);
                }
            };

            let typed = match self.post_chat(&body, &key).await {
                Ok(resp) => {
                    lock(&self.pool).report_success(&key);
                    return Ok(resp);
                }
                Err(e) => e,
            };
            let text = typed.to_string();
            if self.drop_rejected_param(&mut body, &text) {
                continue;
            }
            match &typed {
                EvrenError::TermsNotAccepted { .. } => return Err(typed),
                EvrenError::QuotaExhausted { .. } => {
                    let has = {
                        let mut p = lock(&self.pool);
                        p.report_disabled(&key, "quota_exhausted");
                        p.has_available()
                    };
                    last_error = Some(typed);
                    if has {
                        continue;
                    }
                    break;
                }
                EvrenError::Auth { .. } => {
                    lock(&self.pool).report_disabled(&key, "auth_error");
                    last_error = Some(typed);
                    continue;
                }
                EvrenError::RateLimit { retry_after, .. } => {
                    let ra = *retry_after;
                    let has = {
                        let mut p = lock(&self.pool);
                        p.report_rate_limited(&key, ra);
                        p.has_available()
                    };
                    if has {
                        last_error = Some(typed);
                        continue;
                    }
                    if attempt + 1 < max_retries {
                        let wait = ra
                            .filter(|s| *s > 0.0)
                            .unwrap_or(f64::from(attempt + 1) * 3.0);
                        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                        lock(&self.pool).clear_cooldown(&key);
                        attempt += 1;
                        last_error = Some(typed);
                        continue;
                    }
                    return Err(typed);
                }
                _ => {
                    if typed.retryable() && attempt + 1 < max_retries {
                        let wait = typed
                            .retry_after()
                            .filter(|s| *s > 0.0)
                            .unwrap_or(f64::from(attempt + 1) * 3.0);
                        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                        attempt += 1;
                        last_error = Some(typed);
                        continue;
                    }
                    return Err(typed);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| EvrenError::RateLimit {
            message: texts::NO_KEY_AVAILABLE.to_owned(),
            retry_after: None,
            failed_key: None,
        }))
    }

    async fn post_chat(
        &self,
        body: &Map<String, Value>,
        key: &str,
    ) -> Result<ChatResponse, EvrenError> {
        let resp = self
            .request(reqwest::Method::POST, "/chat/completions", key)
            .json(body)
            .send()
            .await
            .map_err(|e| errors::from_reqwest(&e, Some(key)))?;
        let headers = header_map(resp.headers());
        self.note_chat_headers(&headers, key);
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(errors::from_status(
                status.as_u16(),
                &headers,
                &text,
                Some(key),
            ));
        }
        let raw: Value = resp.json().await.map_err(|e| EvrenError::Api {
            message: format!("Yanıt JSON olarak okunamadı: {e}"),
            status_code: Some(status.as_u16()),
            failed_key: Some(key.to_owned()),
        })?;
        Ok(ChatResponse { raw })
    }

    /// SSE akışlı sohbet tamamlama (`chat_stream_assembled`). Her parça
    /// `on_delta` ile iletilir; sonuç akışsız yanıtla aynı biçimde döner.
    /// Anahtar değişimi, geçici hata denemesi ve yarıda kesilen yanıtı
    /// sürdürme mantığı Python ile aynıdır. Şartlar hatası çağırana bırakılır.
    pub async fn chat_complete_stream<F>(
        &self,
        req: ChatRequest,
        mut on_delta: F,
    ) -> Result<ChatResponse, EvrenError>
    where
        F: FnMut(StreamDelta) + Send,
    {
        let mut body = self.build_body(&req);
        body.insert("stream".into(), Value::Bool(true));
        body.insert("stream_options".into(), json!({"include_usage": true}));
        let idle = if body.get("thinking") == Some(&Value::Bool(false)) {
            STREAM_IDLE_TIMEOUT_NO_THINK
        } else {
            STREAM_IDLE_TIMEOUT
        };
        let base_messages: Vec<Value> = body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let max_attempts = lock(&self.pool).len().max(1);
        let mut attempts = 0usize;
        let mut tries: u32 = 0;
        let mut resumed = false;
        let mut asm = StreamAssembler::new();
        let mut last_error: Option<EvrenError> = None;

        while attempts < max_attempts {
            let key = self.acquire_key_switching().await?;
            let err = match self
                .stream_once(&body, &key, idle, &mut asm, &mut on_delta)
                .await
            {
                Ok(()) => {
                    lock(&self.pool).report_success(&key);
                    return Ok(ChatResponse {
                        raw: asm.into_raw(),
                    });
                }
                Err(e) => e,
            };
            let text = err.to_string();
            if !asm.streamed_any() && self.drop_rejected_param(&mut body, &text) {
                continue;
            }
            match &err {
                EvrenError::TermsNotAccepted { .. } => return Err(err),
                EvrenError::QuotaExhausted { .. } | EvrenError::Auth { .. } => {
                    let reason = if matches!(err, EvrenError::Auth { .. }) {
                        "auth_error"
                    } else {
                        "quota_exhausted"
                    };
                    let has = {
                        let mut p = lock(&self.pool);
                        p.report_disabled(&key, reason);
                        p.has_available()
                    };
                    attempts += 1;
                    last_error = Some(err);
                    if has {
                        continue;
                    }
                    break;
                }
                EvrenError::RateLimit { retry_after, .. } => {
                    let has = {
                        let mut p = lock(&self.pool);
                        p.report_rate_limited(&key, *retry_after);
                        p.has_available()
                    };
                    attempts += 1;
                    last_error = Some(err);
                    if has {
                        continue;
                    }
                    break;
                }
                EvrenError::Network { .. }
                    if asm.streamed_any()
                        && !resumed
                        && asm.tool_calls.is_empty()
                        && !asm.content.is_empty() =>
                {
                    resumed = true;
                    let mut msgs = base_messages.clone();
                    msgs.push(json!({"role": "assistant", "content": asm.content.clone()}));
                    msgs.push(json!({"role": "user", "content": texts::STREAM_RESUME_PROMPT}));
                    body.insert("messages".into(), Value::Array(msgs));
                    continue;
                }
                _ => {
                    if can_retry_transient(&err, asm.streamed_any(), tries) {
                        tries += 1;
                        let wait = transient_backoff(&err, tries);
                        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| EvrenError::RateLimit {
            message: texts::NO_KEY_AVAILABLE.to_owned(),
            retry_after: None,
            failed_key: None,
        }))
    }

    /// Tek bir akış isteği: SSE parçalarını okur, toplayıcıya ve `on_delta`'ya
    /// iletir. Parçalar arası boşta kalma `idle`'ı aşarsa ağ hatası.
    async fn stream_once(
        &self,
        body: &Map<String, Value>,
        key: &str,
        idle: Duration,
        asm: &mut StreamAssembler,
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<(), EvrenError> {
        let resp = self
            .request(reqwest::Method::POST, "/chat/completions", key)
            .timeout(STREAM_TOTAL_TIMEOUT)
            .json(body)
            .send()
            .await
            .map_err(|e| errors::from_reqwest(&e, Some(key)))?;
        let headers = header_map(resp.headers());
        self.note_chat_headers(&headers, key);
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(errors::from_status(
                status.as_u16(),
                &headers,
                &text,
                Some(key),
            ));
        }
        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::new();
        loop {
            let chunk = match tokio::time::timeout(idle, stream.next()).await {
                Err(_) => {
                    return Err(EvrenError::Network {
                        message: texts::STREAM_TIMEOUT_MESSAGE.to_owned(),
                        failed_key: Some(key.to_owned()),
                    })
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(errors::from_reqwest(&e, Some(key))),
                Ok(Some(Ok(bytes))) => bytes,
            };
            for payload in parser.feed(&chunk) {
                for d in asm.apply(&payload)? {
                    on_delta(d);
                }
            }
            if parser.done {
                break;
            }
        }
        if !parser.done {
            for payload in parser.finish() {
                for d in asm.apply(&payload)? {
                    on_delta(d);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_messages_fold() {
        let msgs = vec![
            json!({"role": "user", "content": "a"}),
            json!({"role": "system", "content": "S1"}),
            json!({"role": "system", "content": "S2"}),
            json!({"role": "assistant", "content": "b"}),
        ];
        let out = normalize_messages(&msgs);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[0]["content"], "S1\n\nS2");
        assert_eq!(out[1]["content"], "a");
    }

    #[test]
    fn response_accessors() {
        let r = ChatResponse {
            raw: json!({"choices": [{"message": {"content": "merhaba", "reasoning_content": "düşündüm"}, "finish_reason": "stop"}]}),
        };
        assert_eq!(r.content(), Some("merhaba"));
        assert_eq!(r.reasoning(), Some("düşündüm"));
        assert_eq!(r.finish_reason(), Some("stop"));
        assert!(r.tool_calls().is_none());
    }

    #[test]
    fn transient_helpers() {
        let server = EvrenError::Server {
            message: "503".into(),
            status_code: Some(503),
            failed_key: None,
        };
        assert!(can_retry_transient(&server, false, 0));
        assert!(can_retry_transient(&server, false, 1));
        assert!(!can_retry_transient(&server, false, 2));
        assert!(!can_retry_transient(&server, true, 0));
        let quota = EvrenError::QuotaExhausted {
            message: "q".into(),
            failed_key: None,
        };
        assert!(!can_retry_transient(&quota, false, 0));
        assert!((transient_backoff(&server, 1) - 2.0).abs() < 1e-9);
        assert!((transient_backoff(&server, 2) - 4.0).abs() < 1e-9);
        let rl = EvrenError::RateLimit {
            message: "r".into(),
            retry_after: Some(7.5),
            failed_key: None,
        };
        assert!((transient_backoff(&rl, 1) - 7.5).abs() < 1e-9);
    }
}
