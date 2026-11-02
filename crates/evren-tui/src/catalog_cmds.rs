//! Katalog komutları (Python `repl.py` `_cmd_skills/_cmd_skill/_cmd_diagnose/
//! _cmd_template/_cmd_playbook/_cmd_resume`): /skills /skill /apply /diagnose
//! /template /playbook /resume playbook. Ajana gidecek istem `Some` döner.

use std::path::{Path, PathBuf};

use evren_agent::AgentSession;
use evren_core::catalog::{self, SkillCatalog};
use evren_core::project_docs::cli_docs_dir;

use crate::ui::{print_error, print_info, print_warning};

fn skills_dir(session: &AgentSession) -> PathBuf {
    catalog::user_skills_dir(&cli_docs_dir(session.config()))
}

pub fn skills(session: &AgentSession, arg: &str) {
    let dir = skills_dir(session);
    let cat = SkillCatalog::load(&dir);
    if arg.trim().to_lowercase() == "reload" {
        print_info(&format!(
            "Kullanıcı becerileri yeniden yüklendi: {} adet ({})",
            cat.user_count,
            dir.display()
        ));
        for w in &cat.warnings {
            print_warning(&format!("Atlanan dosya: {w}"));
        }
        return;
    }
    let items = cat.list(Some(arg));
    if items.is_empty() {
        print_warning(&format!(
            "'{arg}' kategorisinde beceri yok. Kategoriler: {}",
            cat.categories().join(", ")
        ));
        return;
    }
    print_info(&format!("Beceriler ({}):", items.len()));
    for s in items {
        let tag = if s.source == "user" { "[user] " } else { "" };
        println!("  • {:<22} [{}] {tag}{}", s.name, s.category, s.title);
    }
    println!("\nUygulamak için: /skill <beceri> (veya /apply <beceri>)");
    println!(
        "Kullanıcı becerileri dizini: {} (yeniden yükleme: /skills reload)",
        dir.display()
    );
}

pub fn skill(session: &AgentSession, arg: &str) -> Option<String> {
    if arg.is_empty() {
        print_warning("Kullanım: /skill <beceri_adı>");
        return None;
    }
    let dir = skills_dir(session);
    match SkillCatalog::load(&dir).get(arg) {
        Some(s) => {
            print_info(&format!("Beceri uygulanıyor: {arg}"));
            Some(s.prompt_block(""))
        }
        None => {
            print_error(&format!(
                "Beceri bulunamadı: {arg}. Kullanıcı becerisi eklemek için: {} altına <ad>.md yazın, sonra /skills reload",
                dir.display()
            ));
            None
        }
    }
}

pub fn diagnose(arg: &str) -> Option<String> {
    if arg.is_empty() {
        print_info("Arıza Triyajları:");
        for d in catalog::list_diagnostics() {
            println!("  • {:<22} {}", d.name, d.title);
        }
        println!("\nÇalıştırmak için: /diagnose <ad>");
        return None;
    }
    if let Some(d) = catalog::get_diagnostic(arg) {
        print_info(&format!("Triyaj başlatılıyor: {}", d.title));
        return Some(d.prompt_block(""));
    }
    let hits = catalog::match_by_symptom(arg);
    if hits.is_empty() {
        print_error(&format!("Triyaj bulunamadı: {arg}"));
    } else {
        print_info("Belirtiye uyan triyajlar:");
        for d in hits {
            println!("  • {:<22} {}", d.name, d.title);
        }
    }
    None
}

pub fn template(arg: &str) -> Option<String> {
    if arg.is_empty() {
        print_info("Çıktı Şablonları:");
        for t in catalog::list_templates(None) {
            println!("  • {:<22} [{}] {}", t.name, t.audience, t.title);
        }
        println!("\nKitleler: {}", catalog::audiences().join(", "));
        println!("Doldurmak için: /template <ad>");
        return None;
    }
    match catalog::get_template(arg) {
        Some(t) => {
            print_info(&format!("Şablon dolduruluyor: {}", t.title));
            Some(t.prompt_block(""))
        }
        None => {
            print_error(&format!("Şablon bulunamadı: {arg}"));
            None
        }
    }
}

pub fn playbook(root: &Path, arg: &str) -> Option<String> {
    if arg.is_empty() {
        print_info("Playbook'lar:");
        for pb in catalog::list_playbooks() {
            println!("  • {:<12} {} ({} adım)", pb.name, pb.title, pb.steps.len());
        }
        println!("\nBaşlatmak için: /playbook <ad>");
        return None;
    }
    let Some(pb) = catalog::get_playbook(arg) else {
        print_error(&format!("Playbook bulunamadı: {arg}"));
        return None;
    };
    if let Err(e) = catalog::save_playbook_checkpoint(root, pb.name, 0) {
        print_error(&e);
        return None;
    }
    print_info(&format!(
        "Playbook başlatıldı: {} (adım 1/{})",
        pb.title,
        pb.steps.len()
    ));
    pb.step_prompt(0, "").ok()
}

/// `/resume playbook`: kaydedilmiş adımın istemi ve sonra yazılacak adım.
pub fn resume_playbook(root: &Path) -> Option<(String, &'static str, usize)> {
    let Some(cp) = catalog::load_playbook_checkpoint(root) else {
        print_warning("Kaydedilmiş playbook adımı yok.");
        return None;
    };
    let name = cp.get("playbook").and_then(|v| v.as_str()).unwrap_or("");
    let step = cp.get("next_step").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    match catalog::get_playbook(name) {
        Some(pb) if step < pb.steps.len() => {
            print_info(&format!(
                "Devam ediliyor: {} (adım {}/{})",
                pb.title,
                step + 1,
                pb.steps.len()
            ));
            Some((pb.step_prompt(step, "").ok()?, pb.name, step + 1))
        }
        _ => {
            print_info("Playbook tamamlanmış görünüyor.");
            None
        }
    }
}
