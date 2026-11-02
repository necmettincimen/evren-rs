//! `AgentEvent` tüketicisi: ajan olaylarını uçbirime basar, onay ve
//! `ask_user` sorularını kullanıcıya sorar (stdio ve başsız kipten sonra
//! üçüncü tüketici; ajanda UI kodu yok).

use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;

use evren_agent::AgentEvent;
use evren_core::settings::{add_allow_rule, rule_for, Args};
use evren_core::texts;
use evren_core::tools::diff::generate_unified_diff;
use evren_core::tools::proc::StreamTag;
use evren_core::tools::{ApprovalReply, ApprovalRequest};
use serde_json::Value;

use crate::input::WatchPause;
use crate::markdown::MarkdownStream;
use crate::ui;

/// Onay sorusunun cevabı.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    /// İzin ver ve kuralı `settings.local.json` dosyasına yaz.
    Always,
}

/// Kullanıcıya soru soran taraf; testlerde sahtesi kullanılır.
pub trait Prompter: Send {
    fn confirm(&mut self, msg: &str, default_yes: bool) -> Decision;
    fn ask(&mut self, question: &str) -> Option<String>;
}

fn headless() -> bool {
    std::env::var("EVREN_HEADLESS")
        .map(|v| v == "1")
        .unwrap_or(false)
        || !std::io::stdin().is_terminal()
}

/// `y/n/a` cevabını çözer; boş cevap varsayılandır.
pub fn parse_decision(answer: &str, default_yes: bool) -> Decision {
    let a = answer.trim().to_lowercase();
    match a.as_str() {
        "" if default_yes => Decision::Allow,
        "" => Decision::Deny,
        "y" | "yes" | "evet" | "e" => Decision::Allow,
        "a" | "hep" => Decision::Always,
        _ => Decision::Deny,
    }
}

/// stdin'den okuyan gerçek soru sorucu (Python `prompt_confirm`, `ask_text`).
pub struct StdinPrompter {
    pause: WatchPause,
}

impl StdinPrompter {
    pub fn new(pause: WatchPause) -> Self {
        Self { pause }
    }

    fn read_line(&self) -> Option<String> {
        self.pause.set(true);
        let mut line = String::new();
        let n = tokio::task::block_in_place(|| std::io::stdin().lock().read_line(&mut line));
        self.pause.set(false);
        match n {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }
}

impl Prompter for StdinPrompter {
    fn confirm(&mut self, msg: &str, default_yes: bool) -> Decision {
        if headless() {
            println!("{}", texts::confirm_headless(msg, default_yes));
            return if default_yes {
                Decision::Allow
            } else {
                Decision::Deny
            };
        }
        let hint = if default_yes { "[Y/n" } else { "[y/N" };
        print!("\n{msg} {hint}/a=hep izin ver]: ");
        ui::flush();
        match self.read_line() {
            Some(line) => parse_decision(&line, default_yes),
            None => {
                println!("{}", texts::confirm_no_input(default_yes));
                parse_decision("", default_yes)
            }
        }
    }

    fn ask(&mut self, question: &str) -> Option<String> {
        if headless() {
            return None;
        }
        print!("\n[Model sorusu] {question}\n> ");
        ui::flush();
        self.read_line()
            .map(|l| l.trim_end_matches(['\r', '\n']).to_string())
    }
}

/// Tur boyunca olayları basar.
pub struct EventPrinter {
    workspace: PathBuf,
    prompter: Box<dyn Prompter>,
    last_call: Option<(String, Value)>,
    md: MarkdownStream,
    reasoning_open: bool,
    /// `always` ile kural eklendi; tur sonunda ayarlar yeniden yüklenmeli.
    pub settings_dirty: bool,
}

impl EventPrinter {
    pub fn new(workspace: PathBuf, prompter: Box<dyn Prompter>) -> Self {
        Self {
            workspace,
            prompter,
            last_call: None,
            md: MarkdownStream::new(ui::color_enabled(), &ui::ui_theme()),
            reasoning_open: false,
            settings_dirty: false,
        }
    }

    /// Akış satırı açıksa kapatır.
    fn close_stream(&mut self) {
        if self.reasoning_open {
            println!();
        }
        self.reasoning_open = false;
        let rest = self.md.finish();
        if !rest.is_empty() {
            print!("{rest}");
            ui::flush();
        }
    }

    pub fn handle(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::ReasoningDelta(chunk) => {
                if self.md.active() {
                    self.close_stream();
                }
                self.reasoning_open = true;
                print!("{}", ui::reasoning_chunk(&chunk));
                ui::flush();
            }
            AgentEvent::AssistantDelta(chunk) => {
                if self.reasoning_open {
                    self.close_stream();
                }
                let out = self.md.push(&chunk);
                if !out.is_empty() {
                    print!("{out}");
                    ui::flush();
                }
            }
            AgentEvent::Reasoning(text) => {
                if self.reasoning_open {
                    self.close_stream();
                } else if !text.trim().is_empty() {
                    self.close_stream();
                    ui::print_reasoning(&text);
                }
            }
            AgentEvent::Assistant(text) => {
                if self.md.active() {
                    self.close_stream();
                } else if !text.trim().is_empty() {
                    self.close_stream();
                    let mut md = MarkdownStream::new(ui::color_enabled(), &ui::ui_theme());
                    let mut out = md.push(&text);
                    out.push_str(&md.finish());
                    print!("{out}");
                }
            }
            AgentEvent::Status(msg) => {
                self.close_stream();
                println!("{msg}");
            }
            AgentEvent::Info(msg) => {
                self.close_stream();
                if ui::looks_like_diff(&msg) {
                    ui::print_diff(&msg, "");
                } else {
                    ui::print_info(&msg);
                }
            }
            AgentEvent::Warning(msg) => {
                self.close_stream();
                ui::print_warning(&msg);
            }
            AgentEvent::Error(msg) => {
                self.close_stream();
                ui::print_error(&msg);
            }
            AgentEvent::ToolCall { name, args, .. } => {
                self.close_stream();
                ui::print_tool_call(&name, &args);
                self.last_call = Some((name, args));
            }
            AgentEvent::ToolResult {
                name, ok, summary, ..
            } => {
                self.close_stream();
                ui::print_tool_result(&name, ok, &summary);
            }
            AgentEvent::ToolOutput { stream, line } => {
                self.close_stream();
                ui::print_command_output(&line, matches!(stream, StreamTag::Stderr));
            }
            AgentEvent::ApprovalRequest { request, reply } => {
                self.close_stream();
                let answer = self.approve(&request);
                let _ = reply.send(answer);
            }
            AgentEvent::AskUser {
                question, reply, ..
            } => {
                self.close_stream();
                let _ = reply.send(self.prompter.ask(&question));
            }
            AgentEvent::RunSummary {
                steps,
                seconds,
                tokens,
            } => {
                self.close_stream();
                ui::print_run_summary(steps, seconds, tokens);
            }
            AgentEvent::Todos(_) | AgentEvent::Usage(_) => {}
        }
    }

    /// Onayı sorar; `always` son araç çağrısının kuralını kalıcı yazar.
    fn approve(&mut self, request: &ApprovalRequest) -> ApprovalReply {
        if !request.detail.trim().is_empty() {
            if ui::looks_like_diff(&request.detail) {
                ui::print_diff(&request.detail, "");
            } else {
                println!("{}", request.detail);
            }
        } else if let Some(p) = &request.proposal {
            ui::print_diff(&generate_unified_diff(&p.old, &p.new, &p.path), &p.path);
        }
        if let Some(w) = &request.warning {
            ui::print_warning(w);
        }
        match self.prompter.confirm(&request.title, request.default_yes) {
            Decision::Allow => ApprovalReply::allow(),
            Decision::Deny => ApprovalReply::deny(),
            Decision::Always => {
                self.remember_current();
                ApprovalReply::allow()
            }
        }
    }

    fn remember_current(&mut self) {
        let Some((name, args)) = &self.last_call else {
            return;
        };
        let empty = Args::new();
        let rule = rule_for(name, args.as_object().unwrap_or(&empty));
        match add_allow_rule(&self.workspace, &rule) {
            Ok(_) => {
                self.settings_dirty = true;
                ui::print_success(&format!(
                    "Kural eklendi (.evren/settings.local.json): {rule}"
                ));
            }
            Err(e) => ui::print_warning(&format!("Kural yazılamadı: {e}")),
        }
    }

    /// Tur sonunda açık akış satırını kapatır.
    pub fn finish(&mut self) {
        self.close_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evren_core::tools::ApprovalKind;
    use serde_json::json;
    use tokio::sync::oneshot;

    struct Fixed(Decision);

    impl Prompter for Fixed {
        fn confirm(&mut self, _msg: &str, _default_yes: bool) -> Decision {
            self.0
        }
        fn ask(&mut self, _question: &str) -> Option<String> {
            Some("cevap".into())
        }
    }

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            kind: ApprovalKind::Command,
            ui_kind: "command",
            title: "Komutu çalıştır?".into(),
            detail: String::new(),
            default_yes: false,
            proposal: None,
            warning: None,
        }
    }

    #[test]
    fn decisions_parse() {
        assert_eq!(parse_decision("", true), Decision::Allow);
        assert_eq!(parse_decision("", false), Decision::Deny);
        assert_eq!(parse_decision("Evet", false), Decision::Allow);
        assert_eq!(parse_decision("a", false), Decision::Always);
        assert_eq!(parse_decision("hayır", true), Decision::Deny);
    }

    #[test]
    fn always_writes_local_rule() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let mut p = EventPrinter::new(dir.path().to_path_buf(), Box::new(Fixed(Decision::Always)));
        p.handle(AgentEvent::ToolCall {
            call_id: "1".into(),
            name: "run_command".into(),
            args: json!({"command": "cargo test"}),
        });
        let (tx, mut rx) = oneshot::channel();
        p.handle(AgentEvent::ApprovalRequest {
            request: request(),
            reply: tx,
        });
        assert!(matches!(rx.try_recv(), Ok(r) if r.approved));
        assert!(p.settings_dirty);
        let text = std::fs::read_to_string(dir.path().join(".evren/settings.local.json"))
            .unwrap_or_default();
        assert!(text.contains("run_command"), "{text}");
    }

    #[test]
    fn deny_and_ask_reply() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let mut p = EventPrinter::new(dir.path().to_path_buf(), Box::new(Fixed(Decision::Deny)));
        let (tx, mut rx) = oneshot::channel();
        p.handle(AgentEvent::ApprovalRequest {
            request: request(),
            reply: tx,
        });
        assert!(matches!(rx.try_recv(), Ok(r) if !r.approved));
        let (tx, mut rx) = oneshot::channel();
        p.handle(AgentEvent::AskUser {
            question: "Hangisi?".into(),
            kind: "text".into(),
            reply: tx,
        });
        assert_eq!(rx.try_recv().ok().flatten().as_deref(), Some("cevap"));
        assert!(!p.settings_dirty);
    }
}
