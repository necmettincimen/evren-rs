//! Takip ve iş akışı komutları (Python `repl.py` `_cmd_track/_cmd_task/
//! _cmd_progress` ve `_handle_workflow_command`): /track /task /progress
//! /checkpoint /blockers /check-in /decisions. Değişiklikler açık onay ister.

use std::io::BufRead;
use std::path::Path;

use evren_agent::AgentSession;
use evren_core::redact::redact_text;
use evren_core::tracking::{self, progress_of, str_field, STATUSES};
use evren_core::workflow_state::{self as wf, blocker_categories, flags_of, BLOCKER_ROUTES};
use serde_json::Value;

use crate::extras::{clear_checkpoint, prompt_confirm};
use crate::ui::{self, print_error, print_info, print_success, print_warning};

/// Python `str.split(maxsplit=n)`: en çok `n` bölme, baştaki/sondaki boşluk atılır.
pub fn split_max(text: &str, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        if out.len() == n {
            out.push(rest.trim_end().to_string());
            break;
        }
        match rest.find(char::is_whitespace) {
            Some(i) => {
                out.push(rest[..i].to_string());
                rest = rest[i..].trim_start();
            }
            None => {
                out.push(rest.to_string());
                break;
            }
        }
    }
    out
}

fn read_line(label: &str) -> String {
    print!("{label}");
    ui::flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim_end_matches(['\r', '\n']).to_string()
}

pub fn track(root: &Path, arg: &str) {
    let filter = if arg.is_empty() { "all" } else { arg };
    let items = tracking::list_tasks(filter, root);
    let summary = tracking::progress_summary(root);
    print_info(&format!(
        "Görevler ({}) — tamamlanma: %{}",
        items.len(),
        summary.percent
    ));
    if items.is_empty() {
        println!("  (görev yok)");
    }
    for t in &items {
        println!(
            "  • [{}] {}: {} (%{})",
            str_field(t, "status"),
            str_field(t, "id"),
            str_field(t, "description"),
            progress_of(t)
        );
    }
    println!("\nKomutlar: /task add <id> <açıklama> | /progress <id> <yüzde>");
}

pub fn task(root: &Path, arg: &str) {
    let parts = split_max(arg, 2);
    if parts.len() < 3 || parts[0].to_lowercase() != "add" {
        print_warning("Kullanım: /task add <id> <açıklama>");
        return;
    }
    match tracking::add_task(&parts[1], &parts[2], "", root) {
        Ok(_) => print_success(&format!("Görev eklendi: {}", parts[1])),
        Err(e) => print_error(&e),
    }
}

pub fn progress(root: &Path, arg: &str) {
    let parts = split_max(arg, 2);
    let pct = parts
        .get(1)
        .filter(|p| p.chars().all(|c| c.is_ascii_digit()));
    let Some(pct) = pct.and_then(|p| p.parse::<i64>().ok()) else {
        print_warning("Kullanım: /progress <id> <yüzde> [not]");
        return;
    };
    let note = parts.get(2).map_or("", String::as_str);
    match tracking::update_task(&parts[0], None, Some(pct), note, root) {
        Ok(item) => print_success(&format!(
            "{} → %{} ({})",
            str_field(&item, "id"),
            progress_of(&item),
            str_field(&item, "status")
        )),
        Err(e) => print_error(&e),
    }
}

/// Tur sonu: duraklamış görev yoksa checkpoint silinir, varsa kayıt önerilir.
pub fn after_turn(session: &AgentSession) {
    if session.pending_mode().is_none() {
        clear_checkpoint(session.workspace_root());
    } else if prompt_confirm(
        "Duraklayan görevi diske kaydedelim mi? Yaygın sırlar maskelenir fakat tam \
         gizlilik garantisi yoktur; hassas görevde reddedin.",
    ) {
        match session.save_checkpoint() {
            Ok(_) => print_success(
                "Checkpoint kaydedildi. Sonraki oturumda /continue ile onaylı yüklenebilir.",
            ),
            Err(e) => print_error(&format!("İş akışı işlemi başarısız: {e}")),
        }
    }
}

/// `/continue` öncesi: boşta bir oturumda kalıcı checkpoint varsa onayla yükler.
/// Kullanıcı reddederse `false` (tur çalışmaz).
pub fn offer_restore(session: &mut AgentSession) -> bool {
    if session.pending_mode().is_some()
        || !session.task_goal().is_empty()
        || !wf::checkpoint_path(session.workspace_root()).exists()
    {
        return true;
    }
    if !prompt_confirm(
        "Kalıcı checkpoint yüklensin mi? Maskelenmiş bilgiler yeniden sağlanmalıdır.",
    ) {
        return false;
    }
    if let Err(e) = session.restore_checkpoint() {
        print_error(&format!("İş akışı işlemi başarısız: {e}"));
        return false;
    }
    true
}

/// /checkpoint /blockers /check-in /decisions.
pub fn command(session: &mut AgentSession, cmd: &str, arg: &str) {
    if let Err(e) = run(session, cmd, arg) {
        print_error(&format!("İş akışı işlemi başarısız: {e}"));
    }
}

fn run(session: &mut AgentSession, cmd: &str, arg: &str) -> Result<(), String> {
    let root = session.workspace_root().to_path_buf();
    match cmd {
        "/checkpoint" => checkpoint(session, &root, arg),
        "/decisions" => decisions(session, &root, arg),
        "/blockers" => blockers(&root, arg),
        _ => check_in(&root, arg),
    }
}

fn checkpoint(session: &AgentSession, root: &Path, arg: &str) -> Result<(), String> {
    match arg {
        "save" => {
            if prompt_confirm(
                "Görev geçmişini maskeleyerek diske kaydet? Hassas içerikte reddedin.",
            ) {
                session.save_checkpoint()?;
                print_success("Checkpoint kaydedildi.");
            }
        }
        "clear" => {
            if prompt_confirm("Kalıcı checkpoint silinsin mi?") {
                clear_checkpoint(root);
                print_success("Kalıcı checkpoint silindi.");
            }
        }
        "" => {
            match wf::load_checkpoint(root)? {
                None => print_info("Kalıcı checkpoint yok."),
                Some(st) => {
                    let s = |k: &str| st.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    print_info(&format!(
                        "Checkpoint: {} | {} | {}",
                        s("saved_at"),
                        s("model"),
                        s("mode")
                    ));
                }
            }
            print_info("Komutlar: /checkpoint save | /checkpoint clear | /continue");
        }
        _ => return Err("Kullanım: /checkpoint [save|clear]".into()),
    }
    Ok(())
}

fn decisions(session: &mut AgentSession, root: &Path, arg: &str) -> Result<(), String> {
    match arg {
        "add" => {
            let decision = read_line("Karar > ");
            let reason = read_line("Gerekçe > ");
            let risk = read_line("Risk ve sınırlar > ");
            if [&decision, &reason, &risk]
                .iter()
                .any(|v| v.trim().is_empty())
            {
                return Err("Karar, gerekçe ve risk boş olamaz.".into());
            }
            if prompt_confirm("Bu karar günlüğe kaydedilsin ve ajan bağlamına eklensin mi?") {
                let item = wf::add_decision(root, &decision, &reason, &risk)?;
                session.refresh_context();
                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                print_success(&format!("Karar kaydedildi: {id}"));
            }
        }
        "" => {
            let items = wf::load_decisions(root)?;
            print_info(&format!("Onaylı kararlar ({}):", items.len()));
            for item in &items {
                let f = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("");
                println!(
                    "  {}: {}\n    Gerekçe: {}\n    Risk: {}",
                    f("id"),
                    f("decision"),
                    f("reason"),
                    f("risk")
                );
            }
            print_info("Yeni kayıt: /decisions add");
        }
        _ => return Err("Kullanım: /decisions [add]".into()),
    }
    Ok(())
}

fn blockers(root: &Path, arg: &str) -> Result<(), String> {
    let parts = split_max(arg, 3);
    let categories = blocker_categories().join(", ");
    match parts.first().map(String::as_str) {
        Some("add") => {
            if parts.len() != 4 {
                return Err("Kullanım: /blockers add <görev-id> <kategori> <kanıt>".into());
            }
            if prompt_confirm("Bu engeli görev kaydına ekle?") {
                tracking::set_blocker(&parts[1], &parts[2], &parts[3], root)?;
                print_success("Engel kaydedildi.");
            }
        }
        Some("resolve") => {
            let pieces = split_max(arg, 2);
            if pieces.len() != 3 {
                return Err("Kullanım: /blockers resolve <görev-id> <çözüm notu>".into());
            }
            if prompt_confirm("Engeli çözüldü olarak işaretle?") {
                tracking::resolve_blocker(&pieces[1], &pieces[2], root)?;
                print_success("Engel çözüldü; görev devam ediyor.");
            }
        }
        _ => {
            if !arg.is_empty() && !BLOCKER_ROUTES.iter().any(|(k, _)| *k == arg) {
                return Err(format!("Kategoriler: {categories}"));
            }
            let items = wf::review_tasks(&tracking::list_tasks("all", root), None)?;
            for item in &items {
                let flags = flags_of(item);
                if !flags.iter().any(|f| f == "blocked" || f == "stalled") {
                    continue;
                }
                let blocker = if str_field(item, "status") == "blocked" {
                    item.get("blocker").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                let b = |k: &str| blocker.get(k).and_then(Value::as_str);
                if !arg.is_empty() && b("category") != Some(arg) {
                    continue;
                }
                print_info(&format!("{} [{}]", str_field(item, "id"), flags.join(", ")));
                println!(
                    "  Kanıt: {}",
                    b("evidence").unwrap_or("Kayıtlı kanıt yok; neden varsayılmadı.")
                );
                println!(
                    "  Öneri: {}",
                    b("route").unwrap_or("Görev durumunu gözden geçir; /check-in kullan.")
                );
            }
            print_info(&format!("Kategoriler: {categories}"));
            print_info("/blockers add <id> <kategori> <kanıt> | /blockers resolve <id> <not>");
        }
    }
    Ok(())
}

fn check_in(root: &Path, arg: &str) -> Result<(), String> {
    if !matches!(arg, "" | "update") {
        return Err("Kullanım: /check-in [update]".into());
    }
    let items = wf::review_tasks(&tracking::list_tasks("all", root), None)?;
    print_info(&format!("Açık görev gözden geçirmesi ({}):", items.len()));
    for item in &items {
        let flags = flags_of(item);
        let id = str_field(item, "id");
        println!(
            "  {id} [{}] %{} | {} | {}",
            str_field(item, "status"),
            progress_of(item),
            if flags.is_empty() {
                "open".to_string()
            } else {
                flags.join(", ")
            },
            str_field(item, "description")
        );
        if arg != "update" {
            continue;
        }
        let status = read_line(&format!(
            "{id} durum ({}; Enter=koru) > ",
            STATUSES.join(", ")
        ))
        .trim()
        .to_string();
        let raw = read_line("İlerleme 0-100 (Enter=koru) > ")
            .trim()
            .to_string();
        let note = read_line("Gözden geçirme notu (Enter=atla) > ")
            .trim()
            .to_string();
        if !status.is_empty() && !STATUSES.contains(&status.as_str()) {
            return Err("Geçersiz görev durumu.".into());
        }
        let pct = if raw.is_empty() {
            None
        } else {
            match raw.parse::<i64>() {
                Ok(p) if raw.chars().all(|c| c.is_ascii_digit()) && (0..=100).contains(&p) => {
                    Some(p)
                }
                _ => return Err("İlerleme 0-100 arasında tam sayı olmalıdır.".into()),
            }
        };
        if let Some(p) = pct {
            if (status == "completed" && p != 100)
                || (!status.is_empty() && status != "completed" && p == 100)
            {
                return Err(
                    "Durum ve ilerleme çelişiyor: completed yalnızca %100 olmalıdır.".into(),
                );
            }
        }
        if (!status.is_empty() || pct.is_some() || !note.is_empty())
            && prompt_confirm(&format!("{id} güncellensin mi?"))
        {
            let st = (!status.is_empty()).then_some(status.as_str());
            tracking::update_task(id, st, pct, &redact_text(&note), root)?;
            print_success(&format!("Görev güncellendi: {id}"));
        }
    }
    print_info("Onaylı güncelleme: /check-in update");
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn split_max_matches_python() {
        assert_eq!(
            split_max("  add T1  bir  iki ", 2),
            vec!["add", "T1", "bir  iki"]
        );
        assert_eq!(split_max("T1 50", 2), vec!["T1", "50"]);
        assert!(split_max("   ", 2).is_empty());
        assert_eq!(split_max("a b c d e", 3), vec!["a", "b", "c", "d e"]);
    }
}
