//! `evren agent`: tek seferlik otonom görev (Python `cli.cmd_agent`).
//! Oturum kaydedilmez; arka plan komutları çıkışta kapatılır.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use evren_agent::prompt_frame::{delivery_summary, PromptFrame};
use evren_agent::AgentSession;
use evren_api::ApiClient;
use evren_core::config::Config;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::events::{EventPrinter, StdinPrompter};
use crate::input::WatchPause;
use crate::ui::{print_error, print_info};

/// Tek seferlik görevin girdileri.
pub struct AgentOnce {
    pub model: Option<String>,
    pub frame: PromptFrame,
    pub prompt: String,
    pub max_steps: Option<u32>,
}

/// Görevi çalıştırır; `HATA:`/`İPTAL:` sonucu 1 döner.
pub async fn run_agent_once(
    client: Arc<ApiClient>,
    config: &Config,
    workspace: &Path,
    task: AgentOnce,
) -> ExitCode {
    // Akıllı bağlam belgeleri sistem istemi kurulmadan önce hazır olmalı.
    crate::docs::bootstrap(config, workspace);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let mut session = AgentSession::new(client, config.clone(), workspace, tx, cancel.clone());
    if let Some(m) = &task.model {
        session.set_model(m);
    }
    let name = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workspace.display().to_string());
    print_info(&format!(
        "Otonom ajan başlatılıyor • {} • {name}",
        session.model()
    ));
    let text = if task.frame.is_empty() {
        task.prompt.clone()
    } else {
        print_info(&format!(
            "Kalıp API'ye ayrıldı: {}",
            delivery_summary(&task.frame)
        ));
        session.apply_frame(&task.frame, &task.prompt, false)
    };
    let ctx = session.tool_ctx().clone();
    let max_steps = task.max_steps;
    let mut printer = EventPrinter::new(
        workspace.to_path_buf(),
        Box::new(StdinPrompter::new(WatchPause::default())),
    );
    let worker = tokio::spawn(async move { session.run_step(&text, max_steps, true).await });
    tokio::pin!(worker);
    let joined = loop {
        tokio::select! {
            Some(event) = rx.recv() => printer.handle(event),
            res = &mut worker => break res,
            _ = tokio::signal::ctrl_c() => cancel.cancel(),
        }
    };
    while let Ok(event) = rx.try_recv() {
        printer.handle(event);
    }
    printer.finish();
    let result = joined.unwrap_or_else(|e| format!("HATA: {e}"));
    if result.starts_with("HATA:") {
        print_error(&result);
    }
    // Tek seferlik kip arka plan komut ağaçlarını öksüz bırakmamalı.
    let killed = ctx.background.lock().map_or(0, |mut bg| bg.cleanup_all());
    if killed > 0 {
        print_info(&format!(
            "Arka planda çalışan {killed} komut kapatılırken durduruldu."
        ));
    }
    ExitCode::from(u8::from(
        result.starts_with("HATA:") || result.starts_with("İPTAL:"),
    ))
}
