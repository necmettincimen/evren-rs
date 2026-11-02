//! Akıllı bağlam belgeleri (`agent.md` / `changelog.md`) için açılış satırı,
//! `/docs` ve `evren docs` (Python `repl._bootstrap_docs`,
//! `repl._handle_docs_command`, `cli.cmd_docs`).

use std::path::Path;
use std::process::ExitCode;

use evren_agent::AgentSession;
use evren_core::config::Config;
use evren_core::project_docs::{
    cli_docs_dir, docs_status, ensure_project_docs, unmark_docs, DocsOptions, DocsStatus,
};

use crate::extras::prompt_confirm_default;
use crate::ui::{print_info, print_success, print_warning};

const UNMARK_WARNING: &str = "Yönetilen bloklar kaldırılır; elle yazdığınız metin kalır. \
                              Sonraki çalıştırmada CLI bu dosyalara dokunmaz.";
const UNMARK_QUESTION: &str = "Yönetilen bloklar kaldırılsın mı?";

/// Açılışta belgeleri oluşturur/yeniler; tek özet satırı + sorun notları.
pub fn bootstrap(config: &Config, workspace: &Path) {
    let result = ensure_project_docs(workspace, config, &DocsOptions::default());
    print_info(&format!("Bağlam: {}", result.summary()));
    for note in result.details() {
        print_warning(&format!("Bağlam: {note}"));
    }
}

fn enabled_line(status: &DocsStatus) -> String {
    let mut line = if status.enabled { "evet" } else { "hayır" }.to_string();
    if !status.reason.is_empty() {
        line.push_str(&format!(" ({})", status.reason));
    }
    line
}

fn changelog_line(status: &DocsStatus) -> String {
    if status.changelog.is_empty() {
        return "yok".to_string();
    }
    format!("{} ({} giriş)", status.changelog, status.changelog_entries)
}

fn or_none(text: &str) -> &str {
    if text.is_empty() {
        "yok"
    } else {
        text
    }
}

fn unmark_summary(workspace: &Path) -> String {
    let result = unmark_docs(workspace);
    if result.updated.is_empty() && result.notes.is_empty() {
        "Belge bulunamadı.".to_string()
    } else {
        result.summary()
    }
}

/// `/docs [status|refresh [--adopt]|unmark|path]`.
pub fn repl_command(session: &mut AgentSession, arg: &str) {
    let root = session.workspace_root().to_path_buf();
    let config = session.config().clone();
    let sub = arg.trim().to_lowercase();
    if sub.is_empty() || sub == "status" {
        let status = docs_status(&root, &config);
        print_info("Akıllı bağlam dosyaları:");
        println!("  • Çalışma alanı   : {}", status.root);
        println!("  • Etkin           : {}", enabled_line(&status));
        println!("  • Ajan belgesi    : {}", or_none(&status.agent_doc));
        if !status.agent_sections.is_empty() {
            println!("  • Yönetilen blok  : {}", status.agent_sections.join(", "));
        }
        println!("  • Changelog       : {}", changelog_line(&status));
        println!("  • Durum dosyası   : {}", or_none(&status.state));
        println!("  • Token bütçesi   : {}", status.token_budget);
        println!("\nKomutlar: /docs refresh [--adopt] | /docs unmark | /docs path");
        return;
    }
    if sub == "path" || sub == "dir" {
        println!("Proje belgeleri : {}", root.display());
        println!("CLI belgeleri   : {}", cli_docs_dir(&config).display());
        return;
    }
    if sub.starts_with("refresh") {
        let adopt = sub.contains("--adopt");
        if !prompt_confirm_default(
            "agent.md yönetilen blokları ve changelog başlığı yenilensin mi?",
            true,
        ) {
            print_info("Vazgeçildi.");
            return;
        }
        let opts = DocsOptions { refresh: true, adopt, ..Default::default() };
        let result = ensure_project_docs(&root, &config, &opts);
        session.refresh_context();
        print_success(&result.summary());
        return;
    }
    if sub == "unmark" {
        print_warning(UNMARK_WARNING);
        if !prompt_confirm_default(UNMARK_QUESTION, false) {
            print_info("Vazgeçildi.");
            return;
        }
        let summary = unmark_summary(&root);
        session.refresh_context();
        print_success(&summary);
        return;
    }
    print_warning("Kullanım: /docs [status|refresh [--adopt]|unmark|path]");
}

/// `evren docs [status|refresh|unmark|path] [--adopt] [--yes]`.
pub fn cli_command(config: &Config, workspace: &Path, action: &str, adopt: bool, yes: bool) -> ExitCode {
    match action {
        "path" => {
            println!("Proje belgeleri : {}", workspace.display());
            println!("CLI belgeleri   : {}", cli_docs_dir(config).display());
        }
        "refresh" => {
            let opts = DocsOptions { refresh: true, adopt, ..Default::default() };
            print_success(&ensure_project_docs(workspace, config, &opts).summary());
        }
        "unmark" => {
            if !yes {
                print_warning(UNMARK_WARNING);
                if !prompt_confirm_default(UNMARK_QUESTION, false) {
                    print_info("Vazgeçildi.");
                    return ExitCode::SUCCESS;
                }
            }
            print_success(&unmark_summary(workspace));
        }
        _ => {
            let status = docs_status(workspace, config);
            print_info("Akıllı bağlam dosyaları:");
            println!("  • Çalışma alanı : {}", status.root);
            println!("  • Etkin         : {}", enabled_line(&status));
            println!("  • Ajan belgesi  : {}", or_none(&status.agent_doc));
            if !status.agent_sections.is_empty() {
                println!("  • Yönetilen blok: {}", status.agent_sections.join(", "));
            }
            println!("  • Changelog     : {}", changelog_line(&status));
            println!("  • CLI belgeleri : {}", cli_docs_dir(config).display());
            println!("  • Token bütçesi : {}", status.token_budget);
        }
    }
    ExitCode::SUCCESS
}
