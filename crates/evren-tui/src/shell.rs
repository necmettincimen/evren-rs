//! `!komut` kabuk kipi (Python `shell_mode.py`).
//!
//! Kullanıcı `!git status` yazınca komut ajanın kabuğunda çalışır, çıktı canlı
//! basılır ve kırpılmış hali geçmişe bilgi notu olarak eklenir; model yanıtı
//! istenmez.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use evren_agent::AgentSession;
use evren_core::tools::command::command_argv;
use evren_core::tools::proc::{stream_with_tree_kill_until, StreamTag};

use crate::ui;

/// Geçmişe eklenen çıktının karakter sınırı.
pub const SHELL_NOTE_CHARS: usize = 4000;
/// `!komut` için zaman aşımı.
pub const SHELL_TIMEOUT: Duration = Duration::from_secs(600);

/// `!komut` satırı mı (tek başına `!` değil).
pub fn is_shell_input(text: &str) -> bool {
    let stripped = text.trim_start();
    stripped.starts_with('!') && stripped.trim_end().chars().count() > 1
}

/// Uzun çıktının ortasını kırpar (baş ve son 2000 karakter kalır).
pub fn clip_output(lines: &[String]) -> String {
    let body = lines.join("\n");
    let n = body.chars().count();
    if n <= SHELL_NOTE_CHARS {
        return body;
    }
    let half = SHELL_NOTE_CHARS / 2;
    let head: String = body.chars().take(half).collect();
    let tail: String = body.chars().skip(n - half).collect();
    format!(
        "{head}\n... [{} satırın ortası kırpıldı] ...\n{tail}",
        lines.len()
    )
}

/// Geçmişe eklenen kullanıcı notu.
pub fn shell_note(command: &str, code: i32, body: &str) -> String {
    format!(
        "[Kullanıcının kendi çalıştırdığı kabuk komutu — bilgi için, yanıt gerekmez]\n\
         $ {command}\nÇıkış kodu: {code}\n{body}"
    )
}

/// `!komut` çalıştırır, sonucu ajan için kaydeder; çıkış kodunu döner.
pub async fn run_user_shell(text: &str, session: &mut AgentSession, timeout: Duration) -> i32 {
    let command = text.trim_start()[1..].trim().to_string();
    let argv = match command_argv(session.tool_ctx(), &command, false, false) {
        Ok(a) => a,
        Err(e) => {
            ui::print_warning(&e);
            return 1;
        }
    };
    let mut cmd = argv.to_command();
    cmd.current_dir(session.workspace_root());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let mut lines = Vec::new();
        let outcome = stream_with_tree_kill_until(&mut cmd, Some(timeout), &flag, |tag, line| {
            ui::print_command_output(line, matches!(tag, StreamTag::Stderr));
            lines.push(line.trim_end_matches(['\n', '\r']).to_string());
        });
        (outcome, lines)
    });
    let joined = loop {
        tokio::select! {
            res = &mut worker => break res,
            _ = tokio::signal::ctrl_c() => stop.store(true, Ordering::Relaxed),
        }
    };
    let (outcome, lines) = match joined {
        Ok(pair) => pair,
        Err(e) => {
            ui::print_warning(&e.to_string());
            return 1;
        }
    };
    let mut code = match outcome {
        Ok(o) => {
            if o.timed_out && !stop.load(Ordering::Relaxed) {
                ui::print_warning(&format!("Komut {} sn sonra durduruldu.", timeout.as_secs()));
            }
            o.code
        }
        Err(e) => {
            ui::print_warning(&e.to_string());
            return 1;
        }
    };
    if stop.load(Ordering::Relaxed) {
        ui::print_warning("Komut durduruldu (Ctrl+C).");
        code = 130;
    }
    ui::print_info(&format!("Çıkış kodu: {code}"));
    let body = clip_output(&lines);
    let body = match session.vault().lock() {
        Ok(vault) => vault.mask(&body),
        Err(_) => body,
    };
    session.push_user_message(&shell_note(&command, code, &body));
    code
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn detection() {
        for (text, want) in [
            ("!git status", true),
            ("  !ls", true),
            ("!", false),
            ("merhaba !", false),
        ] {
            assert_eq!(is_shell_input(text), want, "{text}");
        }
    }

    #[test]
    fn long_output_is_clipped_in_the_middle() {
        let lines: Vec<String> = (0..500)
            .map(|i| format!("satır {i:04} {}", "x".repeat(20)))
            .collect();
        let body = clip_output(&lines);
        assert!(body.starts_with("satır 0000"));
        assert!(body.ends_with(&lines[499]));
        assert!(body.contains("[500 satırın ortası kırpıldı]"));
        assert!(body.chars().count() < SHELL_NOTE_CHARS + 100);
        assert_eq!(clip_output(&["kısa".to_string()]), "kısa");
    }

    #[tokio::test]
    async fn output_is_shown_and_remembered_for_the_agent() {
        let ws = tempfile::tempdir().unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("EVREN_API_KEY".to_string(), "sk-test-dummy".to_string());
        env.insert("EVREN_DEFAULT_MODEL".to_string(), "fake-model".to_string());
        let config = evren_core::config::Config::from_env(env);
        let client = Arc::new(evren_api::ApiClient::new(&config).unwrap());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = AgentSession::new(
            client,
            config,
            ws.path(),
            tx,
            tokio_util::sync::CancellationToken::new(),
        );
        let code = run_user_shell("!echo evren-ok", &mut session, SHELL_TIMEOUT).await;
        assert_eq!(code, 0);
        let last = session.messages().last().unwrap();
        assert_eq!(last["role"], "user");
        let content = last["content"].as_str().unwrap();
        assert!(content.contains("evren-ok"), "{content}");
        assert!(content.contains("Çıkış kodu: 0"));
        assert!(content.contains("$ echo evren-ok"));
    }
}
