//! Kullanıcıya görünen Türkçe metinler. Python kaynağındaki dizgelerle
//! birebir aynıdır; test uyumu için buradan değiştirilir.

pub const API_KEY_MISSING: &str =
    "EVREN_API_KEY bulunamadı. Lütfen .env dosyanızı veya ortam değişkeninizi ayarlayın.";
pub const UI_THEME_INVALID: &str = "EVREN_UI_THEME yalnızca light veya dark olabilir.";

pub const KEY_SOURCE_PROCESS_ENV: &str = "süreç ortam değişkeni (EVREN_API_KEY / EVREN_API_KEYS)";
pub const KEY_SOURCE_NOT_FOUND: &str = "bulunamadı";

pub const RATE_LIMIT_DEFAULT: &str = "Rate limit aşıldı (429).";
pub const QUOTA_EXHAUSTED_DEFAULT: &str = "Günlük token kotası tükendi.";
pub const AUTH_DEFAULT: &str = "Kimlik doğrulama/yetkilendirme hatası.";
pub const SERVER_DEFAULT: &str = "Sunucu hatası (5xx).";
pub const NETWORK_DEFAULT: &str = "Ağ bağlantı hatası.";
pub const TERMS_DEFAULT: &str = "EVREN kullanım şartları kabul edilmemiş.";
pub const NO_KEY_AVAILABLE: &str = "İstek tamamlanamadı: kullanılabilir API anahtarı yok.";
pub const QUOTA_UNKNOWN: &str = "Bilinmiyor";

pub const CHAT_SYSTEM: &str =
    "Sen Windows üzerinde çalışan uzman bir yazılım mühendisi asistanısın.";
pub const MODELS_FETCHING: &str = "EVREN API modelleri çekiliyor...";
pub const MODELS_HEADER: &str = "EVREN API Modelleri:";
pub const NOT_PORTED_YET: &str = "Bu komut henüz Rust sürümüne taşınmadı.";

// --stdio hata metinleri (stdio_server.py)
pub const STDIO_INVALID_MESSAGE: &str = "Mesaj 'type' alanı olan bir JSON nesnesi olmalı.";
pub const STDIO_UNSUPPORTED_VERSION: &str = "Desteklenen protokol sürümü: 1";
pub const STDIO_BUSY: &str = "Ajan çalışıyor; önce cancel gönderin.";
pub const STDIO_EMPTY_PROMPT: &str = "Boş istek.";

pub fn stdio_invalid_json(e: &dyn std::fmt::Display) -> String {
    format!("Geçersiz JSON: {e}")
}
pub fn stdio_unknown_command(kind: &str) -> String {
    format!("Bilinmeyen komut: {kind}")
}
pub fn pre_tool_hook_blocked(code: i32) -> String {
    format!("pre_tool hook engelledi (çıkış {code})")
}
pub fn hook_error(e: &dyn std::fmt::Display) -> String {
    format!("hook hatası: {e}")
}
pub fn model_thinking(model: &str) -> String {
    format!("Model düşünüyor... ({model})")
}
