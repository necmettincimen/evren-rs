//! Model kayıtları (`models.py`) ve çıktı token tabanı (`token_manager.py`).

/// Düşünme akışı üreten modeller: çıktı için en az 4096 token ayrılır.
pub const REASONING_MODELS: [&str; 3] = ["deepseek-v4.1-flash", "glm-5.3", "qwen3.8-flash-next"];
pub const REASONING_MIN_OUTPUT_TOKENS: u32 = 4096;
pub const QWEN_FLASH_MIN_OUTPUT_TOKENS: u32 = 8192;
pub const DEFAULT_CONTEXT_WINDOW: u32 = 128_000;

/// `thinking: false` gönderildiğinde düşünme metnini içeriğe sızdıran modeller.
pub const THINKING_OFF_UNSAFE_MODELS: [&str; 2] = ["glm-5.3", "zai/glm-5.3-fp8"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Planner,
    Executor,
    Summarizer,
    Vision,
    Classifier,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Executor => "executor",
            Role::Summarizer => "summarizer",
            Role::Vision => "vision",
            Role::Classifier => "classifier",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "planner" => Some(Role::Planner),
            "executor" => Some(Role::Executor),
            "summarizer" => Some(Role::Summarizer),
            "vision" => Some(Role::Vision),
            "classifier" => Some(Role::Classifier),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    pub name: &'static str,
    pub roles: &'static [Role],
    pub context_window: u32,
    pub thinking_off_ok: bool,
    pub vision: bool,
}

/// Kayıt sırası önemlidir: `default_model_for` ilk eşleşeni döndürür.
pub const REGISTRY: [ModelInfo; 5] = [
    ModelInfo {
        name: "qwen3.8-flash-next",
        roles: &[Role::Executor, Role::Summarizer, Role::Vision],
        context_window: 256_000,
        thinking_off_ok: true,
        vision: true,
    },
    ModelInfo {
        name: "deepseek-v4.1-flash",
        roles: &[Role::Planner, Role::Executor, Role::Summarizer],
        context_window: 1_000_000,
        thinking_off_ok: true,
        vision: false,
    },
    ModelInfo {
        name: "mimo-v2.6-pro",
        roles: &[Role::Planner, Role::Executor],
        context_window: 1_000_000,
        thinking_off_ok: true,
        vision: false,
    },
    ModelInfo {
        name: "glm-5.3",
        roles: &[Role::Planner, Role::Executor],
        context_window: 512_000,
        thinking_off_ok: false,
        vision: false,
    },
    ModelInfo {
        name: "clef",
        roles: &[Role::Classifier],
        context_window: 8_000,
        thinking_off_ok: true,
        vision: false,
    },
];

/// Küçük harfe indirilmiş ad içinde kayıt adını arar (`zai/glm-5.3-fp8` → glm-5.3).
pub fn lookup(model: &str) -> Option<&'static ModelInfo> {
    let low = model.trim().to_lowercase();
    if low.is_empty() {
        return None;
    }
    REGISTRY.iter().find(|m| low.contains(m.name))
}

pub fn context_window(model: &str) -> u32 {
    lookup(model)
        .map(|m| m.context_window)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

/// Bilinmeyen modeller için `true` (Python ile aynı).
pub fn thinking_off_ok(model: &str) -> bool {
    lookup(model).map(|m| m.thinking_off_ok).unwrap_or(true)
}

/// `config.thinking_off_supported`: boş ad ya da güvensiz liste → false.
pub fn thinking_off_supported(model: &str) -> bool {
    if model.is_empty() || THINKING_OFF_UNSAFE_MODELS.contains(&model) {
        return false;
    }
    thinking_off_ok(model)
}

pub fn supports_vision(model: &str) -> bool {
    lookup(model).map(|m| m.vision).unwrap_or(false)
}

pub fn default_model_for(role: Role, fallback: &str) -> String {
    REGISTRY
        .iter()
        .find(|m| m.roles.contains(&role))
        .map(|m| m.name.to_owned())
        .unwrap_or_else(|| fallback.to_owned())
}

pub fn is_reasoning_model(model: &str) -> bool {
    REASONING_MODELS.contains(&model)
}

/// İstenen `max_tokens` değerini modelin tabanına çeker.
pub fn floor_output_tokens(model: &str, requested: u32) -> u32 {
    if model == "qwen3.8-flash-next" {
        requested.max(QWEN_FLASH_MIN_OUTPUT_TOKENS)
    } else if is_reasoning_model(model) {
        requested.max(REASONING_MIN_OUTPUT_TOKENS)
    } else {
        requested
    }
}

/// `evren models` çıktısındaki öneri açıklamaları (`RECOMMENDED_MODELS`).
pub const RECOMMENDED_MODELS: [(&str, &str); 10] = [
    (
        "qwen3.8-flash-next",
        "Varsayılan · Hızlı Yanıtlar, Vibe Coding, Yüksek Eşzamanlılık",
    ),
    (
        "deepseek-v4.1-flash",
        "Kodlama ve Ajan İşleri (1M Bağlam, Hızlı)",
    ),
    (
        "glm-5.3",
        "Derin Akıl Yürütme ve Çok Adımlı Karmaşık Analiz",
    ),
    ("auto", "Otomatik Model Yönlendirme"),
    ("dots-ocr", "Belge ve Resim Metin Tanıma (OCR)"),
    ("deepseek-ocr-2", "Gelişmiş Belge/Görsel Analizi ve OCR"),
    ("qwen3-asr-1.7b", "Ses Tanıma ve Metne Dönüştürme (ASR)"),
    ("qwen3-embedding-8b", "Vektör Gömme ve Semantik Arama"),
    ("qwen3-reranker-8b", "Arama Sonuçları Yeniden Sıralama"),
    (
        "qwen3-guard-4b",
        "Güvenlik ve Zararlı İçerik Sınıflandırması",
    ),
];

pub fn recommendation(model: &str) -> Option<&'static str> {
    RECOMMENDED_MODELS
        .iter()
        .find(|(m, _)| *m == model)
        .map(|(_, d)| *d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_tokens() {
        assert_eq!(floor_output_tokens("qwen3.8-flash-next", 100), 8192);
        assert_eq!(floor_output_tokens("qwen3.8-flash-next", 9000), 9000);
        assert_eq!(floor_output_tokens("glm-5.3", 100), 4096);
        assert_eq!(floor_output_tokens("mimo-v2.6-pro", 100), 100);
    }

    #[test]
    fn registry_lookup() {
        assert_eq!(context_window("zai/glm-5.3-fp8"), 512_000);
        assert_eq!(context_window("bilinmeyen"), DEFAULT_CONTEXT_WINDOW);
        assert!(thinking_off_ok("bilinmeyen"));
        assert!(!thinking_off_ok("glm-5.3"));
        assert!(!thinking_off_supported(""));
        assert!(!thinking_off_supported("zai/glm-5.3-fp8"));
        assert!(thinking_off_supported("qwen3.8-flash-next"));
        assert_eq!(default_model_for(Role::Planner, "x"), "deepseek-v4.1-flash");
        assert_eq!(
            default_model_for(Role::Summarizer, "x"),
            "qwen3.8-flash-next"
        );
        assert!(supports_vision("qwen3.8-flash-next"));
    }
}
