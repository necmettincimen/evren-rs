//! Görev kalıbı (Python `prompt_frame.py`): Rol/Çerçeve sistem iletisine,
//! Brief/Kısıt/Çıktı kullanıcı iletisine gider. Etkileşimli form TUI'dedir;
//! burada yalnız metin birleştirme ve "devam" algısı bulunur.

use regex::Regex;
use std::sync::OnceLock;

pub const ROLE_NOTE: &str =
    "Rol, hangi kimlikten yazacağını belirler ve genel asistan sesinin önüne geçer.";
pub const FRAME_NOTE: &str = "Çerçeve bu işin kalıbıdır; strateji yalnızca bu satıra yüklenir.";
pub const BRIEF_NOTE: &str = "Brief tek cümlelik niyettir; fazlasını ekleme.";
pub const CONSTRAINT_NOTE: &str = "Kısıt, çerçevedeki stratejiyi görünür tutan ölçektir.";
pub const OUTPUT_NOTE: &str = "Çıktı, format ve teslim yüzeyidir.";

/// Boş alanlar atılır; `rol`/`cerceve` turlar arasında kalıcıdır (`/clear`'a dek).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptFrame {
    pub rol: String,
    pub cerceve: String,
    pub brief: String,
    pub kisit: String,
    pub cikti: String,
    pub clarifications: String,
}

impl PromptFrame {
    pub fn is_empty(&self) -> bool {
        self.rol.is_empty()
            && self.cerceve.is_empty()
            && self.brief.is_empty()
            && self.kisit.is_empty()
            && self.cikti.is_empty()
    }
}

const FOLLOWUP_PHRASES: &[&str] = &[
    "yap",
    "yap artık",
    "artık yap",
    "doğrudan yap",
    "soru sorma doğrudan yap",
    "uygula",
    "başla",
    "tüm görevleri yap",
    "hepsini yap",
    "soru sorma",
    "onay veriyorum",
    "kabul ediyorum",
    "devam et",
    "onaylıyorum",
    "kalan işleri tamamla",
    "kalanları tamamla",
];

fn punct_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[.!?,;]+")
            .unwrap_or_else(|_| Regex::new("^$").unwrap_or_else(|_| unreachable!()))
    })
}

fn approval_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^(?:hepsi|hepsine|hepsi için|tüm görevler için|tüm görevleri yap hepsine|tümüne|tamamını) (?:için )?onay veriyorum(?: kabul ediyorum)?$",
        )
        .unwrap_or_else(|_| Regex::new("^$").unwrap_or_else(|_| unreachable!()))
    })
}

/// Noktalama atılmış, küçük harfe indirilmiş, boşlukları tekilleştirilmiş biçim.
pub fn normalize_followup(text: &str) -> String {
    let lowered = text.to_lowercase();
    let stripped = punct_re().replace_all(&lowered, " ");
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Kısa onay/devam yanıtı mı? Sınıflandırıcı (clef) bu sürümde yok; yalnız sabit liste.
pub fn is_execution_followup(text: &str) -> bool {
    let normalized = normalize_followup(text);
    if FOLLOWUP_PHRASES.contains(&normalized.as_str()) {
        return true;
    }
    approval_re().is_match(&normalized)
}

/// Sistem tarafı blok; Rol ve Çerçeve boşsa boş dize.
pub fn identity_block(frame: &PromptFrame) -> String {
    let mut notes = Vec::new();
    let mut lines = Vec::new();
    if !frame.rol.is_empty() {
        notes.push(ROLE_NOTE.to_string());
        lines.push(format!("Rol: {}", frame.rol));
    }
    if !frame.cerceve.is_empty() {
        notes.push(FRAME_NOTE.to_string());
        lines.push(format!("Çerçeve: {}", frame.cerceve));
    }
    if lines.is_empty() {
        return String::new();
    }
    notes.extend(lines);
    format!("GÖREV KALIBI\n{}", notes.join("\n"))
}

pub fn compose_system(base: &str, frame: &PromptFrame) -> String {
    let block = identity_block(frame);
    if block.is_empty() {
        return base.to_string();
    }
    format!("{}\n\n{}", base.trim_end(), block)
}

/// Kullanıcı tarafı görev metni; boş alanlar atılır, `extra` serbest ektir.
pub fn build_user_content(frame: &PromptFrame, extra: &str) -> String {
    let mut notes = Vec::new();
    let mut lines = Vec::new();
    if !frame.brief.is_empty() {
        notes.push(BRIEF_NOTE.to_string());
        lines.push(format!("Brief: {}", frame.brief));
    }
    if !frame.kisit.is_empty() {
        notes.push(CONSTRAINT_NOTE.to_string());
        lines.push(format!("Kısıt: {}", frame.kisit));
    }
    if !frame.cikti.is_empty() {
        notes.push(OUTPUT_NOTE.to_string());
        lines.push(format!("Çıktı: {}", frame.cikti));
    }
    let mut chunks: Vec<String> = Vec::new();
    if !lines.is_empty() {
        notes.extend(lines);
        chunks.push(format!("GÖREV\n{}", notes.join("\n")));
    }
    let extra = extra.trim();
    if !extra.is_empty() {
        if chunks.is_empty() {
            chunks.push(extra.to_string());
        } else {
            chunks.push(format!("Ek istek:\n{extra}"));
        }
    }
    if !frame.clarifications.is_empty() {
        chunks.push(format!(
            "PROMPT HAZIRLIĞINDA NETLEŞTİRİLEN BİLGİLER:\n{}",
            frame.clarifications
        ));
    }
    if chunks.is_empty() {
        return "Verilen rol ve çerçeveye göre ilerle.".to_string();
    }
    chunks.join("\n\n")
}

/// Gönderilecek alanların kısa yönlendirme özeti.
pub fn delivery_summary(frame: &PromptFrame) -> String {
    let mut parts = Vec::new();
    if !frame.rol.is_empty() {
        parts.push("Rol→system");
    }
    if !frame.cerceve.is_empty() {
        parts.push("Çerçeve→system");
    }
    if !frame.brief.is_empty() {
        parts.push("Brief→user");
    }
    if !frame.kisit.is_empty() {
        parts.push("Kısıt→user");
    }
    if !frame.cikti.is_empty() {
        parts.push("Çıktı→user");
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(rol: &str, cerceve: &str, brief: &str, kisit: &str, cikti: &str) -> PromptFrame {
        PromptFrame {
            rol: rol.into(),
            cerceve: cerceve.into(),
            brief: brief.into(),
            kisit: kisit.into(),
            cikti: cikti.into(),
            clarifications: String::new(),
        }
    }

    #[test]
    fn identity_on_system_task_on_user() {
        let f = frame("mühendis", "yama", "timeout 45", "yalnız api", "kod");
        let sys = compose_system("TEMEL\n", &f);
        assert!(sys.starts_with("TEMEL\n\nGÖREV KALIBI\n"));
        assert!(sys.contains("Rol: mühendis"));
        assert!(sys.contains("Çerçeve: yama"));
        assert!(!sys.contains("Brief:"));
        let user = build_user_content(&f, "");
        assert!(user.starts_with("GÖREV\n"));
        assert!(user.contains("Brief: timeout 45"));
        assert!(user.contains("Kısıt: yalnız api"));
        assert!(user.contains("Çıktı: kod"));
        assert!(!user.contains("Rol:"));
        assert_eq!(
            delivery_summary(&f),
            "Rol→system, Çerçeve→system, Brief→user, Kısıt→user, Çıktı→user"
        );
    }

    #[test]
    fn empty_frame_leaves_system_untouched() {
        let f = PromptFrame::default();
        assert!(f.is_empty());
        assert_eq!(compose_system("TEMEL", &f), "TEMEL");
        assert_eq!(identity_block(&f), "");
        assert_eq!(
            build_user_content(&f, ""),
            "Verilen rol ve çerçeveye göre ilerle."
        );
    }

    #[test]
    fn extra_is_appended_as_ek_istek() {
        let f = frame("", "", "brief", "", "");
        let user = build_user_content(&f, "  ek  ");
        assert!(user.ends_with("\n\nEk istek:\nek"));
        let only_extra = build_user_content(&PromptFrame::default(), "serbest");
        assert_eq!(only_extra, "serbest");
    }

    #[test]
    fn clarifications_block_follows() {
        let mut f = frame("", "", "brief", "", "");
        f.clarifications = "S: a\nC: b".into();
        let user = build_user_content(&f, "");
        assert!(user.contains("PROMPT HAZIRLIĞINDA NETLEŞTİRİLEN BİLGİLER:\nS: a\nC: b"));
    }

    #[test]
    fn execution_followup_phrases() {
        assert!(is_execution_followup("Yap artık!"));
        assert!(is_execution_followup("onay veriyorum."));
        assert!(is_execution_followup(
            "Hepsi için onay veriyorum, kabul ediyorum"
        ));
        assert!(is_execution_followup("tümüne onay veriyorum"));
        assert!(!is_execution_followup("/exit"));
        assert!(!is_execution_followup("timeout süresini 45 saniyeye çıkar"));
        assert!(!is_execution_followup(""));
    }
}
