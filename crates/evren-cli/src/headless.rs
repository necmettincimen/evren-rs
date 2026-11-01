//! Başsız kip: `evren -p "<görev>" [--output json] [--max-steps N]
//! [--allowed-tools a,b] [--yes] [--continue]` (Python `extras.run_headless`).
//!
//! `--output json` iken stdout yalnız tek satırlık sonuç nesnesidir; akış
//! metni ve bilgi satırları stderr'e gider. Onaylar sorulmaz: Python
//! `prompt_confirm` başsız kipte varsayılanı seçer (düzenleme/yazma evet,
//! komut hayır), `ask_user` cevapsız kalır.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use evren_agent::mentions::expand_mentions;
use evren_agent::sessions::{dumps_py, list_sessions};
use evren_agent::{AgentEvent, AgentSession};
use evren_api::ApiClient;
use evren_core::config::Config;
use evren_core::{texts, EvrenError};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// `-p` seçenekleri.
#[derive(Debug, Clone, Default)]
pub struct HeadlessOptions {
    pub prompt: String,
    pub model: Option<String>,
    pub json: bool,
    pub max_steps: Option<u32>,
    pub yes: bool,
    pub allowed_tools: Vec<String>,
    pub continue_session: bool,
}

/// Görevi çalıştırır; dönüş kodu 0 (başarı) ya da 1 (`HATA:`/`İPTAL:`).
pub async fn run(
    config: &Config,
    opts: HeadlessOptions,
    workspace: &Path,
) -> Result<ExitCode, EvrenError> {
    let client = Arc::new(ApiClient::new(config)?);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut session = AgentSession::new(
        client,
        config.clone(),
        workspace,
        tx,
        CancellationToken::new(),
    );
    session.set_auto_approve(opts.yes);
    if !opts.allowed_tools.is_empty() {
        session.set_extra_allow(opts.allowed_tools.clone());
    }
    if let Some(model) = &opts.model {
        session.set_model(model);
    }
    if opts.continue_session {
        if let Some(last) = list_sessions(workspace, 1).first() {
            session.load_session(last);
        }
    }

    let (text, _warnings) = expand_mentions(&opts.prompt, workspace);
    let before = crate::stdio::status_lines(workspace).await;
    let json_mode = opts.json;
    let max_steps = opts.max_steps;
    let worker = tokio::spawn(async move {
        let result = session.run_step(&text, max_steps, true).await;
        (session, result)
    });
    tokio::pin!(worker);
    let outcome = loop {
        tokio::select! {
            Some(ev) = rx.recv() => on_event(ev),
            res = &mut worker => break res,
        }
    };
    while let Ok(ev) = rx.try_recv() {
        on_event(ev);
    }
    // Python: istisna → "HATA: {e}", kod 1 (oturum kaydedilmez).
    let (session, result) = match outcome {
        Ok(pair) => (Some(pair.0), pair.1),
        Err(e) => (None, format!("HATA: {e}")),
    };
    let code = u8::from(result.starts_with("HATA:") || result.starts_with("İPTAL:"));

    if json_mode {
        let after = crate::stdio::status_lines(workspace).await;
        let changed: Vec<Value> = after
            .iter()
            .filter(|ln| !before.contains(ln))
            .map(|ln| crate::stdio::status_path(ln))
            .filter(|p| !crate::stdio::is_evren_file(p))
            .map(Value::from)
            .collect();
        let tokens = session
            .as_ref()
            .and_then(|s| s.token_report().get("session_usage").cloned())
            .unwrap_or(Value::Null);
        let model = session
            .as_ref()
            .map(|s| s.model().to_string())
            .or_else(|| opts.model.clone())
            .unwrap_or_else(|| config.default_model());
        let payload = json!({
            "result": result,
            "changed_files": changed,
            "tokens": tokens,
            "exit_code": code,
            "model": model,
        });
        println!("{}", dumps_py(&payload));
    } else if !result.is_empty() {
        println!("{result}");
    }
    Ok(ExitCode::from(code))
}

/// Olayları yazar; onay ve sorular varsayılanla cevaplanır.
fn on_event(ev: AgentEvent) {
    match ev {
        // stdout yalnız sonuca ayrılır; akış metni her iki kipte stderr'e gider.
        AgentEvent::AssistantDelta(t) => eprint!("{t}"),
        AgentEvent::Assistant(_) => eprintln!(),
        AgentEvent::Status(t) | AgentEvent::Info(t) | AgentEvent::Warning(t) => eprintln!("{t}"),
        AgentEvent::Error(t) => eprintln!("[X] {t}"),
        AgentEvent::ToolOutput { line, .. } => eprintln!("{line}"),
        AgentEvent::ApprovalRequest { request, reply } => {
            eprintln!(
                "{}",
                texts::confirm_headless(&request.title, request.default_yes)
            );
            let _ = reply.send(request.default_yes.into());
        }
        AgentEvent::AskUser { reply, .. } => {
            let _ = reply.send(None);
        }
        AgentEvent::ReasoningDelta(_)
        | AgentEvent::Reasoning(_)
        | AgentEvent::ToolCall { .. }
        | AgentEvent::ToolResult { .. }
        | AgentEvent::Todos(_)
        | AgentEvent::Usage(_)
        | AgentEvent::RunSummary { .. } => {}
    }
}
