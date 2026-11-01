//! EVREN LLM API istemcisi (Python `api_client.py`, `api_key_pool.py`,
//! `errors.py`, `terms.py` eşdeğeri): akışsız ve SSE akışlı sohbet,
//! model listesi, kota, anahtar havuzu ve kullanım şartları.

pub mod client;
pub mod errors;
pub mod key_pool;
pub mod quota;
pub mod sse;
pub mod terms;

pub use client::{
    can_retry_transient, transient_backoff, ApiClient, BestKey, ChatRequest, ChatResponse,
    FALLBACK_MODELS, TRANSIENT_RETRIES,
};
pub use key_pool::{mask_key, ApiKeyPool, KeyState, StateCounts};
pub use quota::normalize_quota;
pub use sse::{SseParser, StreamAssembler, StreamDelta};
pub use terms::{as_bool, ensure_terms_accepted, ensure_terms_for_keys, TermsOptions, TermsUi};
