//! Playbook, çıktı şablonu, beceri ve triyaj katalogları (Python: playbooks.py,
//! templates.py, skills.py, diagnostics.py). Yerleşik veri `catalog_data.rs`
//! içindedir; kullanıcı becerileri `<cli_home>/skills/*.md` dosyalarından okunur.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::catalog_data::{DIAGNOSTIC_DATA, PLAYBOOK_DATA, SKILL_DATA, TEMPLATE_DATA};
use crate::config::get_evren_dir;

fn with_context(mut lines: Vec<String>, context: &str) -> Vec<String> {
    let context = context.trim();
    if !context.is_empty() {
        lines.push(format!("Bağlam: {context}"));
    }
    lines
}

// --- Playbook'lar ------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Playbook {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub steps: &'static [&'static str],
}

impl Playbook {
    /// Tek bir adımın (0 tabanlı) yönerge bloğu.
    pub fn step_prompt(&self, step_index: usize, context: &str) -> Result<String, String> {
        let Some(step) = self.steps.get(step_index) else {
            return Err(format!("Adım aralık dışı: {step_index}"));
        };
        let lines = vec![
            format!("PLAYBOOK: {} ({})", self.title, self.name),
            format!("Amaç: {}", self.description),
            format!("Adım {}/{}: {step}", step_index + 1, self.steps.len()),
        ];
        Ok(with_context(lines, context).join("\n"))
    }
}

fn playbooks() -> impl Iterator<Item = Playbook> {
    PLAYBOOK_DATA
        .iter()
        .map(|&(name, title, description, steps)| Playbook {
            name,
            title,
            description,
            steps,
        })
}

pub fn list_playbooks() -> Vec<Playbook> {
    let mut items: Vec<Playbook> = playbooks().collect();
    items.sort_by_key(|p| p.name);
    items
}

/// Önce tam ada, sonra tek eşleşen başlığa göre çözer (büyük/küçük harf duyarsız).
pub fn get_playbook(name: &str) -> Option<Playbook> {
    let key = name.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    if let Some(p) = playbooks().find(|p| p.name == key) {
        return Some(p);
    }
    unique(playbooks().filter(|p| p.title.to_lowercase() == key))
}

fn unique<T>(mut it: impl Iterator<Item = T>) -> Option<T> {
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(first)
}

fn playbook_checkpoint_path(root: &Path) -> PathBuf {
    get_evren_dir(root).join("playbook.json")
}

pub fn load_playbook_checkpoint(root: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(playbook_checkpoint_path(root)).ok()?;
    let data: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let has_name = match data.get("playbook") {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Null) | None => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    };
    (data.is_object() && has_name).then_some(data)
}

pub fn save_playbook_checkpoint(
    root: &Path,
    name: &str,
    next_step: usize,
) -> Result<Value, String> {
    let pb = get_playbook(name).ok_or_else(|| format!("Playbook bulunamadı: {name}"))?;
    if next_step > pb.steps.len() {
        return Err(format!(
            "Adım aralık dışı: {next_step} (0..{})",
            pb.steps.len()
        ));
    }
    let data = json!({
        "schema_version": 1,
        "playbook": pb.name,
        "next_step": next_step,
        "total_steps": pb.steps.len(),
        "updated_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string(),
    });
    let path = playbook_checkpoint_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(data)
}

pub fn clear_playbook_checkpoint(root: &Path) -> bool {
    std::fs::remove_file(playbook_checkpoint_path(root)).is_ok()
}

// --- Çıktı şablonları --------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Template {
    pub name: &'static str,
    pub title: &'static str,
    pub audience: &'static str,
    pub description: &'static str,
    pub body: &'static str,
}

impl Template {
    pub fn prompt_block(&self, context: &str) -> String {
        let lines = vec![
            format!("ÇIKTI ŞABLONU: {} ({})", self.title, self.name),
            format!("Hedef kitle: {}", self.audience),
            format!("Amaç: {}", self.description),
            "Aşağıdaki iskeleti eksiksiz doldur; boş alan bırakma, gereksiz bölüm ekleme. \
             Kod/kanıt gerektiğinde gerçek dosya ve komut çıktılarına dayan."
                .to_string(),
            String::new(),
            "--- ŞABLON ---".to_string(),
            self.body.trim().to_string(),
            "--- /ŞABLON ---".to_string(),
        ];
        with_context(lines, context).join("\n")
    }
}

fn templates() -> impl Iterator<Item = Template> {
    TEMPLATE_DATA
        .iter()
        .map(|&(name, title, audience, description, body)| Template {
            name,
            title,
            audience,
            description,
            body,
        })
}

pub fn list_templates(audience: Option<&str>) -> Vec<Template> {
    let aud = audience.map(|a| a.trim().to_lowercase());
    let mut items: Vec<Template> = templates()
        .filter(|t| {
            aud.as_ref()
                .map_or(true, |a| a.is_empty() || t.audience.to_lowercase() == *a)
        })
        .collect();
    items.sort_by_key(|t| (t.audience, t.name));
    items
}

pub fn get_template(name: &str) -> Option<Template> {
    let key = name.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    if let Some(t) = templates().find(|t| t.name == key) {
        return Some(t);
    }
    unique(templates().filter(|t| t.title.to_lowercase() == key))
}

pub fn audiences() -> Vec<&'static str> {
    let mut items: Vec<&str> = templates().map(|t| t.audience).collect();
    items.sort_unstable();
    items.dedup();
    items
}

// --- Beceriler -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub title: String,
    pub category: String,
    pub description: String,
    pub when_to_use: String,
    pub steps: Vec<String>,
    /// "builtin" | "user"
    pub source: &'static str,
}

impl Skill {
    pub fn prompt_block(&self, context: &str) -> String {
        let mut lines = vec![
            format!("UYGULANAN BECERİ: {} ({})", self.title, self.name),
            format!("Amaç: {}", self.description),
            format!("Ne zaman: {}", self.when_to_use),
            "Adımlar:".to_string(),
        ];
        lines.extend(
            self.steps
                .iter()
                .enumerate()
                .map(|(i, s)| format!("  {}. {s}", i + 1)),
        );
        with_context(lines, context).join("\n")
    }
}

fn builtin_skills() -> impl Iterator<Item = Skill> {
    SKILL_DATA
        .iter()
        .map(|&(name, title, category, description, when, steps)| Skill {
            name: name.into(),
            title: title.into(),
            category: category.into(),
            description: description.into(),
            when_to_use: when.into(),
            steps: steps.iter().map(|s| s.to_string()).collect(),
            source: "builtin",
        })
}

pub const USER_SKILLS_DIRNAME: &str = "skills";

pub fn user_skills_dir(cli_home: &Path) -> PathBuf {
    cli_home.join(USER_SKILLS_DIRNAME)
}

fn normalize_skill_name(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .replace('_', "-")
        .replace('-', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// Tek beceri dosyasını çözer; ya beceri ya uyarı döner.
pub fn parse_skill_file(path: &Path) -> Result<Skill, String> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("{file_name}: okunamadı ({e})"))?;
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut meta: BTreeMap<String, String> = BTreeMap::new();
    let mut body_start = 0;
    if lines.first().map(|l| l.trim()) == Some("---") {
        if let Some(end) = (1..lines.len()).find(|&i| lines[i].trim() == "---") {
            for raw in &lines[1..end] {
                let raw = raw.trim();
                if raw.is_empty() || !raw.contains(':') || raw.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = raw.split_once(':') {
                    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                    meta.insert(key.trim().to_lowercase(), value.to_string());
                }
            }
            body_start = end + 1;
        }
    }
    let mut steps: Vec<String> = Vec::new();
    for raw in &lines[body_start..] {
        let s = raw.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        if let Some(rest) = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
            steps.push(rest.trim().to_string());
        } else if s.chars().count() > 2 && s.chars().next().is_some_and(char::is_numeric) {
            let cut = [s.find('.'), s.find(')')].into_iter().flatten().min();
            match cut {
                Some(cut) => {
                    let step = s[cut + 1..].trim();
                    if !step.is_empty() {
                        steps.push(step.to_string());
                    }
                }
                None => steps.push(s.to_string()),
            }
        } else {
            steps.push(s.to_string());
        }
    }
    let meta_get = |k: &str| meta.get(k).cloned().unwrap_or_default();
    let mut name = normalize_skill_name(&meta_get("name"));
    if name.is_empty() {
        let stem = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        name = normalize_skill_name(&stem);
    }
    if name.is_empty() {
        return Err(format!("{file_name}: beceri adı türetilemedi, atlandı"));
    }
    if steps.is_empty() && meta_get("description").is_empty() {
        return Err(format!("{file_name}: adım veya açıklama yok, atlandı"));
    }
    let title = Some(meta_get("title"))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| name.clone());
    let category = Some(meta_get("category").trim().to_string())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "user".into());
    Ok(Skill {
        name,
        title,
        category,
        description: meta_get("description"),
        when_to_use: meta_get("when_to_use"),
        steps,
        source: "user",
    })
}

/// Dizindeki tüm `*.md` beceri dosyalarını okur; bozuk dosyalar uyarıyla atlanır.
pub fn load_user_skills(dir: &Path) -> (Vec<Skill>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    for path in files {
        match parse_skill_file(&path) {
            Ok(s) => skills.push(s),
            Err(w) => warnings.push(w),
        }
    }
    (skills, warnings)
}

/// Yerleşik ve kullanıcı becerilerinin birleşimi (aynı adlı kullanıcı becerisi gölgeler).
#[derive(Debug, Clone, Default)]
pub struct SkillCatalog {
    pub skills: BTreeMap<String, Skill>,
    pub warnings: Vec<String>,
    pub user_count: usize,
}

impl SkillCatalog {
    pub fn load(user_dir: &Path) -> Self {
        let mut skills: BTreeMap<String, Skill> =
            builtin_skills().map(|s| (s.name.clone(), s)).collect();
        let (user, warnings) = load_user_skills(user_dir);
        let mut names: Vec<&str> = user.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let user_count = names.len();
        for s in user {
            skills.insert(s.name.clone(), s);
        }
        Self {
            skills,
            warnings,
            user_count,
        }
    }

    pub fn list(&self, category: Option<&str>) -> Vec<&Skill> {
        let cat = category
            .map(|c| c.trim().to_lowercase())
            .filter(|c| !c.is_empty());
        let mut items: Vec<&Skill> = self
            .skills
            .values()
            .filter(|s| {
                cat.as_ref()
                    .map_or(true, |c| s.category.to_lowercase() == *c)
            })
            .collect();
        items.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
        items
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        let key = name.trim().to_lowercase();
        if key.is_empty() {
            return None;
        }
        if let Some(s) = self.skills.get(&key) {
            return Some(s);
        }
        let norm = key.replace([' ', '_'], "-");
        unique(
            self.skills
                .values()
                .filter(|s| s.name == norm || s.title.to_lowercase() == key),
        )
    }

    pub fn categories(&self) -> Vec<&str> {
        let mut items: Vec<&str> = self.skills.values().map(|s| s.category.as_str()).collect();
        items.sort_unstable();
        items.dedup();
        items
    }
}

// --- Arıza triyajları ------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Diagnostic {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub entry_symptoms: &'static [&'static str],
    pub triage_questions: &'static [&'static str],
    pub possible_diagnoses: &'static [&'static str],
    pub routes_to_skills: &'static [&'static str],
    pub routes_to_playbooks: &'static [&'static str],
}

impl Diagnostic {
    pub fn prompt_block(&self, context: &str) -> String {
        let mut lines = vec![
            format!("TRIYAJ: {} ({})", self.title, self.name),
            format!("Amaç: {}", self.description),
            "Giriş belirtileri:".to_string(),
        ];
        lines.extend(self.entry_symptoms.iter().map(|s| format!("  - {s}")));
        lines.push("Triyaj soruları (sırayla sor, cevaplara göre daralt):".into());
        lines.extend(
            self.triage_questions
                .iter()
                .enumerate()
                .map(|(i, q)| format!("  {}. {q}", i + 1)),
        );
        for (label, items) in [
            ("Olası tanılar: ", self.possible_diagnoses),
            ("İlgili beceriler: ", self.routes_to_skills),
            ("İlgili playbook'lar: ", self.routes_to_playbooks),
        ] {
            if !items.is_empty() {
                lines.push(format!("{label}{}", items.join(", ")));
            }
        }
        let mut lines = with_context(lines, context);
        lines.push(
            "Önce kanıt topla (dosya oku, komut çalıştır), sonra en olası tanıyı \
             seç ve ilgili beceri/playbook ile ilerle. Tahminle tanı koyma."
                .into(),
        );
        lines.join("\n")
    }
}

fn diagnostics() -> impl Iterator<Item = Diagnostic> {
    DIAGNOSTIC_DATA.iter().map(
        |&(name, title, description, symptoms, questions, diagnoses, skills, playbooks)| {
            Diagnostic {
                name,
                title,
                description,
                entry_symptoms: symptoms,
                triage_questions: questions,
                possible_diagnoses: diagnoses,
                routes_to_skills: skills,
                routes_to_playbooks: playbooks,
            }
        },
    )
}

pub fn list_diagnostics() -> Vec<Diagnostic> {
    let mut items: Vec<Diagnostic> = diagnostics().collect();
    items.sort_by_key(|d| d.name);
    items
}

pub fn get_diagnostic(name: &str) -> Option<Diagnostic> {
    let key = name.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    if let Some(d) = diagnostics().find(|d| d.name == key) {
        return Some(d);
    }
    unique(diagnostics().filter(|d| d.title.to_lowercase() == key))
}

/// Giriş belirtilerinden biri metinde geçen triyajlar (kayıt sırasıyla).
pub fn match_by_symptom(text: &str) -> Vec<Diagnostic> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return Vec::new();
    }
    diagnostics()
        .filter(|d| {
            d.entry_symptoms
                .iter()
                .any(|s| text.contains(&s.to_lowercase()))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn playbook_lookup_and_prompt() {
        let pb = get_playbook("Release").unwrap();
        assert_eq!(get_playbook("sürüm çıkarma").unwrap().name, "release");
        let p = pb.step_prompt(0, " ctx ").unwrap();
        assert!(p.starts_with("PLAYBOOK: Sürüm Çıkarma (release)\n"));
        assert!(p.contains("Adım 1/5: "));
        assert!(p.ends_with("Bağlam: ctx"));
        assert!(pb.step_prompt(5, "").is_err());
        assert!(get_playbook("").is_none());
        assert_eq!(list_playbooks()[0].name, "hotfix");
    }

    #[test]
    fn playbook_checkpoint_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_playbook_checkpoint(dir.path()).is_none());
        let saved = save_playbook_checkpoint(dir.path(), "hotfix", 2).unwrap();
        assert_eq!(saved["total_steps"], 5);
        assert_eq!(
            load_playbook_checkpoint(dir.path()).unwrap()["next_step"],
            2
        );
        assert!(save_playbook_checkpoint(dir.path(), "hotfix", 6).is_err());
        assert!(save_playbook_checkpoint(dir.path(), "yok", 0).is_err());
        assert!(clear_playbook_checkpoint(dir.path()));
        assert!(!clear_playbook_checkpoint(dir.path()));
    }

    #[test]
    fn templates_and_diagnostics() {
        let t = get_template("adr").unwrap();
        let p = t.prompt_block("");
        assert!(p.contains("--- ŞABLON ---\n# ADR-[NNN]"));
        assert!(p.ends_with("--- /ŞABLON ---"));
        assert_eq!(audiences(), vec!["ops", "team", "user"]);
        assert_eq!(list_templates(Some("OPS")).len(), 1);
        assert_eq!(match_by_symptom("CI kırmızı oldu").len(), 1);
        let d = get_diagnostic("build-failure").unwrap();
        assert!(d.prompt_block("").ends_with("Tahminle tanı koyma."));
    }

    #[test]
    fn user_skills_shadow_and_parse() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("My_Skill.md"),
            "---\ntitle: \"Benim\"\ncategory: \n---\n# Başlık\n- bir\n2) iki\n3. üç\nserbest\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("test-first.md"),
            "---\ndescription: gölge\n---\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("bos.md"), "").unwrap();
        let cat = SkillCatalog::load(dir.path());
        assert_eq!(cat.user_count, 2);
        assert_eq!(
            cat.warnings,
            vec!["bos.md: adım veya açıklama yok, atlandı"]
        );
        let s = cat.get("my skill").unwrap();
        assert_eq!(s.title, "Benim");
        assert_eq!(s.category, "user");
        assert_eq!(s.steps, vec!["bir", "iki", "üç", "serbest"]);
        assert_eq!(cat.get("test-first").unwrap().source, "user");
        assert!(cat
            .get("root-cause-debug")
            .unwrap()
            .prompt_block("")
            .contains("  1. "));
        assert!(cat.categories().contains(&"user"));
    }
}
