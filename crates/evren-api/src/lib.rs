//! EVREN LLM API istemcisi (Python `api_client.py`, `api_key_pool.py`,
//! `errors.py` eşdeğeri). Akış (SSE) desteği sonraki fazda eklenecek.

pub mod client;
pub mod errors;
pub mod key_pool;
pub mod quota;

pub use client::{ApiClient, ChatRequest, ChatResponse, FALLBACK_MODELS};
pub use key_pool::{mask_key, ApiKeyPool, KeyState, StateCounts};
pub use quota::normalize_quota;
