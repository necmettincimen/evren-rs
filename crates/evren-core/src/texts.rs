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
pub fn model_thinking_stream(model: &str) -> String {
    format!("Model düşünüyor ve yanıt hazırlanıyor... ({model})")
}
pub fn api_error(kind: &str, e: &dyn std::fmt::Display) -> String {
    format!("API Hatası ({kind}): {e}")
}

// Akış (api_client.chat_stream_assembled)
pub const STREAM_TIMEOUT_MESSAGE: &str = "Request timed out.";
pub const STREAM_RESUME_PROMPT: &str =
    "Yanıt yarıda kesildi; tam kaldığın yerden devam et, tekrar etme.";

// Anahtar seçimi (cli._pin_best_key)
pub fn key_quota_unreadable(e: &dyn std::fmt::Display) -> String {
    format!("Anahtar kotaları okunamadı; ilk anahtarla devam: {e}")
}
pub fn active_key(masked: &str, remaining: &str) -> String {
    format!("Aktif API anahtarı: {masked} (kalan günlük kota: {remaining})")
}
pub const REASONING_HEADER: &str = "💭 Düşünme / Akıl Yürütme:";

// Kullanım şartları (terms.py)
pub const TERMS_KEY_MISSING: &str =
    "EVREN_API_KEY tanımlanmamış. Lütfen .env veya /keys add ile anahtar ekleyin.";
pub const TERMS_KEY_INVALID: &str = "EVREN API anahtarı geçersiz (401 Unauthorized).";
pub const TERMS_NON_INTERACTIVE: &str = "Etkileşimsiz modda kullanım şartları kabul edilemez. Lütfen 'evren terms' veya REPL içinde /terms çalıştırın.";
pub const TERMS_EMPTY_TEXT: &str = "(Şart metni boş döndü; yine de kabul edebilirsiniz.)";
pub const TERMS_REJECTED_HINT: &str =
    "Kullanım şartları kabul edilmediği sürece model çağrıları 403 hatası alacaktır.";
pub const TERMS_NO_KEYS: &str = "Kontrol edilecek API anahtarı yok.";

pub fn terms_status_failed(code: u16, text: &dyn std::fmt::Display) -> String {
    format!("Şartlar durumu alınamadı ({code}): {text}")
}
pub fn terms_connect_failed(e: &dyn std::fmt::Display) -> String {
    format!("EVREN API sunucusuna bağlanılamadı: {e}")
}
pub fn terms_text_failed(e: &dyn std::fmt::Display) -> String {
    format!("Kullanım şartları metni alınamadı: {e}")
}
pub fn terms_accept_failed(e: &dyn std::fmt::Display) -> String {
    format!("Kullanım şartları onaylanamadı: {e}")
}
pub fn terms_check_failed(e: &dyn std::fmt::Display) -> String {
    format!("Kullanım şartları kontrolü yapılamadı: {e}")
}
pub fn terms_already_accepted(v: i64) -> String {
    format!("Kullanım şartları zaten kabul edilmiş (v{v}).")
}
pub fn terms_not_accepted_yet(v: i64) -> String {
    format!("EVREN LLM API kullanım şartları (v{v}) henüz kabul edilmemiş.")
}
pub fn terms_title(v: i64) -> String {
    format!("EVREN LLM API Kullanım Şartları (Sürüm {v})")
}
pub fn terms_text_unreadable(e: &dyn std::fmt::Display) -> String {
    format!("Kullanım şartları metni okunamadı: {e}")
}
pub fn terms_confirm(v: i64) -> String {
    format!("EVREN LLM Kullanım Şartları v{v}'i kabul ediyor musunuz?")
}
pub fn terms_accepted_ok(v: i64) -> String {
    format!("Kullanım şartları başarıyla kabul edildi (v{v})!")
}
pub fn terms_accept_op_failed(e: &dyn std::fmt::Display) -> String {
    format!("Kabul işlemi başarısız: {e}")
}
pub fn terms_checking_key(masked: &str) -> String {
    format!("Şartlar kontrolü: {masked}")
}

// ui.prompt_confirm
pub fn confirm_headless(msg: &str, default_yes: bool) -> String {
    let d = if default_yes { "evet" } else { "hayır" };
    format!("\n{msg} ->{d} (başsız kip, varsayılan)")
}
pub fn confirm_prompt(msg: &str, default_yes: bool) -> String {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    format!("\n{msg} {hint}: ")
}
pub fn confirm_no_input(default_yes: bool) -> String {
    let d = if default_yes { "evet" } else { "hayır" };
    format!("(girdi yok, varsayılan: {d})")
}

// stdio_server (Faz 2c)
pub const STDIO_INVALID_DECISION: &str = "decision şunlardan biri olmalı: allow, deny, always";
pub const STDIO_UNKNOWN_APPROVAL: &str = "Bekleyen böyle bir onay yok.";
pub const STDIO_UNKNOWN_QUESTION: &str = "Bekleyen böyle bir soru yok.";
pub const STDIO_MODEL_EMPTY: &str = "Model adı boş.";
pub const STDIO_INVALID_MODE: &str = "Kip: normal | plan | ask";
pub fn stdio_unknown_session(sid: &str) -> String {
    format!("Oturum yok: {sid}")
}
pub fn stdio_session_unreadable(sid: &str) -> String {
    format!("Oturum okunamadı: {sid}")
}
pub fn stdio_unsupported_slash(command: &str) -> String {
    format!("/{command} stdio kipinde desteklenmiyor.")
}

// extras.expand_mentions
pub const MENTION_ATTACHED_HEADER: &str = "[Eklenen dosyalar]";
pub fn mention_line_limit_note(limit: usize) -> String {
    format!("\n... [{limit} satır sınırı]")
}
pub fn mention_image(rel: &str) -> String {
    format!("[@{rel}] görsel dosya: içeriği için view_file aracını kullan.")
}
pub fn mention_not_found(rel: &str) -> String {
    format!("@{rel} bulunamadı.")
}
pub fn mention_unreadable(rel: &str, e: &dyn std::fmt::Display) -> String {
    format!("@{rel} okunamadı: {e}")
}
pub fn mention_block_header(rel: &str, first: usize, last: usize) -> String {
    format!("[@{rel} satır {first}-{last}]")
}
