//! Model kayıtları (`models.py`) ve çıktı token tabanı (`token_manager.py`).

/// Düşünme akışı üreten modeller: çıktı için en az 4096 token ayrılır.
pub const REASONING_MODELS: [&str; 3] = ["deepseek-v4.1-flash", "glm-5.3", "qwen3.8-flash-next"];
pub const REASONING_MIN_OUTPUT_TOKENS: u32 = 4096;
pub const QWEN_FLASH_MIN_OUTPUT_TOKENS: u32 = 8192;
pub const DEFAULT_CONTEXT_WINDOW: u32 = 128_000;

/// `thinking: false` gönderildiğinde düşünme metnini içeriğe sızdıran modeller.
pub const THINKING_OFF_UNSAFE_MODELS: [&str; 2] = ["glm-5.3", "zai/glm-5.3-fp8"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    pub name: &'static str,
    pub context_window: u32,
    pub thinking_off_ok: bool,
    pub vision: bool,
}

/// Filo kaydı (evren.ssyz.org.tr/llm-inference). `lookup` alt dize ile
/// eşlediği için bir ad diğerinin parçası olmamalı.
pub const REGISTRY: [ModelInfo; 7] = [
    ModelInfo {
        name: "deepseek-v4.1-flash",
        context_window: 1_000_000,
        thinking_off_ok: true,
        vision: true,
    },
    ModelInfo {
        name: "qwen3.8-flash-next",
        context_window: 256_000,
        thinking_off_ok: true,
        vision: true,
    },
    ModelInfo {
        name: "gemma-4-31b",
        context_window: 256_000,
        thinking_off_ok: true,
        vision: true,
    },
    ModelInfo {
        name: "mimo-v2.6-pro",
        context_window: 1_000_000,
        thinking_off_ok: true,
        vision: true,
    },
    ModelInfo {
        name: "glm-5.3",
        context_window: 512_000,
        thinking_off_ok: false,
        vision: false,
    },
    ModelInfo {
        name: "qwen3-vl-30b",
        context_window: 256_000,
        thinking_off_ok: true,
        vision: true,
    },
    ModelInfo {
        name: "clef",
        context_window: 16_000,
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
pub const RECOMMENDED_MODELS: [(&str, &str); 13] = [
    (
        "deepseek-v4.1-flash",
        "Kod ve Ajan İşleri (1M Bağlam; Yoğun Saatlerde Yavaş)",
    ),
    (
        "qwen3.8-flash-next",
        "Hızlı Yanıtlar, Vibe Coding, Yedek Model",
    ),
    ("gemma-4-31b", "Düşük Gecikmeli Sohbet ve Görsel Okuma"),
    ("glm-5.3", "Derin Akıl Yürütme (Yavaş, ~20 tok/s)"),
    (
        "mimo-v2.6-pro",
        "Varsayılan · Uzun Soluklu Ajan (Emekliye Ayrılabilir)",
    ),
    ("auto", "Otomatik Model Yönlendirme"),
    ("qwen3-vl-30b", "Görsel Akıl Yürütme"),
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
        assert_eq!(context_window("clef"), 16_000);
        assert!(supports_vision("qwen3.8-flash-next"));
        assert!(supports_vision("deepseek/deepseek-v4.1-flash"));
        assert!(!supports_vision("glm-5.3"));
    }
}
