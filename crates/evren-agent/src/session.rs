//! Ajan oturumu: durum, olay kanalı ve döngü dışı yardımcılar
//! (Python `agent.py` `Agent` sınıfının durum/yardımcı bölümü).
//!
//! Adım döngüsünün kendisi `loop.rs` içindedir ([`AgentSession::run_step`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evren_api::ApiClient;
use evren_core::config::Config;
use evren_core::error::EvrenError;
use evren_core::models::context_window;
use evren_core::secret_vault::SecretVault;
use evren_core::settings::{Permission, Settings};
use evren_core::tools::proc::StreamTag;
use evren_core::tools::{ApprovalRequest, Approver, InfoSink, Mode, OutputSink, ToolContext};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::compaction::{truncate_tool_content, HistoryCompactor};
use crate::context::{compose_system, git_context_block, SYSTEM_PROMPT};
use crate::guards::{RepeatGuard, TASK_NOISE_PREFIXES};
use crate::route::ExecProfile;
use crate::todos::TodoStore;
use crate::token_manager::TokenManager;
use crate::trace::TaskTrace;
use crate::usage_log::log_usage;

/// Git özetinin tek çağrı zaman aşımı (Python `git_context_block(timeout=2.0)`).
pub const GIT_BLOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Ajanın ön yüze gönderdiği olaylar (REPL, `--stdio`, headless).
#[derive(Debug)]
pub enum AgentEvent {
    /// Spinner metni (`Model düşünüyor... [model]`).
    Status(String),
    Info(String),
    Warning(String),
    Error(String),
    /// Akıştan gelen düşünme parçası.
    ReasoningDelta(String),
    /// Akıştan gelen yanıt parçası.
    AssistantDelta(String),
    /// Akışta gelmeyen tam düşünme metni.
    Reasoning(String),
    /// Akışta gelmeyen tam yanıt metni (`print_markdown`).
    Assistant(String),
    ToolCall {
        call_id: String,
        name: String,
        args: Value,
    },
    ToolResult {
        call_id: String,
        name: String,
        ok: bool,
        summary: String,
        detail: String,
    },
    /// Canlı komut çıktısı satırı.
    ToolOutput {
        stream: StreamTag,
        line: String,
    },
    /// Onay sorusu; cevap `reply` ile döner (düşürülürse ret).
    ApprovalRequest {
        request: ApprovalRequest,
        reply: oneshot::Sender<bool>,
    },
    /// `ask_user` aracı; `None` = cevapsız.
    AskUser {
        question: String,
        kind: String,
        reply: oneshot::Sender<Option<String>>,
    },
    Todos(Vec<Value>),
    /// Son isteğin sunucu kullanım sayaçları.
    Usage(Value),
    /// Görev sonu tek satır özeti.
    RunSummary {
        steps: u32,
        seconds: f64,
        tokens: i64,
    },
}

pub type EventSender = mpsc::UnboundedSender<AgentEvent>;

/// Bir `view_file` kaydı: aynı aralık değişmeden yeniden okunursa kısa not döner.
#[derive(Debug, Clone)]
pub struct ReadRecord {
    pub digest: String,
    pub step: u32,
    pub call_id: String,
}

/// Değişiklik günlüğü için araç çağrısı kaydı (`_note_tool_call`).
#[derive(Debug, Clone, Default)]
pub struct ToolTraceRecord {
    pub name: String,
    pub ok: bool,
    pub path: Option<String>,
    pub command: Option<String>,
    pub exit_ok: bool,
    pub summary: String,
}

/// Oturum kullanım toplamları (`usage_totals`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

/// Python `Agent` durumu.
pub struct AgentSession {
    pub(crate) client: Arc<ApiClient>,
    pub(crate) config: Config,
    pub(crate) workspace_root: PathBuf,
    pub(crate) model: String,
    pub(crate) system_prompt: String,
    pub(crate) git_block: String,
    pub(crate) auto_approve: bool,
    pub(crate) messages: Vec<Value>,
    pub(crate) token_manager: TokenManager,
    pub(crate) window_model: String,
    pub(crate) compactor: HistoryCompactor,
    pub(crate) reads: HashMap<String, ReadRecord>,
    pub(crate) step_no: u32,
    pub(crate) last_usage: Option<Value>,
    pub(crate) usage_totals: UsageTotals,
    pub(crate) usage_requests: u32,
    pub(crate) compaction_notified: bool,
    /// `/steps` ile verilen oturum tavanı; yoksa `EVREN_MAX_STEPS`.
    pub(crate) max_steps: Option<u32>,
    pub(crate) thinking: Option<bool>,
    pub(crate) exec_profile: Option<ExecProfile>,
    pub(crate) vault: SecretVault,
    pub(crate) listing_done: bool,
    pub(crate) guard: RepeatGuard,
    pub(crate) todos: TodoStore,
    pub(crate) mode: Mode,
    pub(crate) pending_mode: Option<Mode>,
    pub(crate) interrupted: bool,
    pub(crate) task_goal: String,
    pub(crate) tool_trace: Vec<ToolTraceRecord>,
    pub(crate) trace_writer: Option<TaskTrace>,
    pub(crate) api_retries: u32,
    pub(crate) task_start: Option<Instant>,
    pub(crate) task_usage_base: i64,
    pub(crate) todo_mark: u32,
    /// Kayıtlı oturum dosyası damgası (`YYYYMMDD-HHMMSS`); bir kez üretilir.
    pub(crate) session_stamp: Option<String>,
    pub(crate) settings: Settings,
    pub(crate) events: EventSender,
    pub(crate) cancel: CancellationToken,
    pub(crate) tool_ctx: Arc<ToolContext>,
}

impl AgentSession {
    /// Yeni oturum; sistem iletisi ve git özeti burada bir kez oluşturulur.
    pub fn new(
        client: Arc<ApiClient>,
        config: Config,
        workspace_root: &Path,
        events: EventSender,
        cancel: CancellationToken,
    ) -> Self {
        let workspace_root = evren_core::tools::file_ops::resolve_path(workspace_root);
        let model = config.default_model();
        let git_block = git_context_block(&workspace_root, GIT_BLOCK_TIMEOUT);
        let system_prompt = compose_system(SYSTEM_PROMPT, &workspace_root, &config, &git_block);
        let auto_approve = config.auto_approve();
        let tool_ctx = Arc::new(build_tool_context(
            &workspace_root,
            &config,
            events.clone(),
            cancel.clone(),
        ));
        let compactor = HistoryCompactor::new(
            config.smart_compact_enabled(),
            config.compact_target_ratio(),
        )
        .with_summarizer(summarizer_for(client.clone(), config.summary_model()));
        let settings = Settings::load(Some(&workspace_root));
        AgentSession {
            client,
            token_manager: TokenManager::new(context_window(&model)),
            window_model: model.clone(),
            model,
            messages: vec![json!({"role": "system", "content": system_prompt})],
            system_prompt,
            git_block,
            auto_approve,
            compactor,
            reads: HashMap::new(),
            step_no: 0,
            last_usage: None,
            usage_totals: UsageTotals::default(),
            usage_requests: 0,
            compaction_notified: false,
            max_steps: None,
            thinking: None,
            exec_profile: None,
            vault: SecretVault::new(),
            listing_done: false,
            guard: RepeatGuard::new(),
            todos: TodoStore::new(),
            mode: Mode::Normal,
            pending_mode: None,
            interrupted: false,
            task_goal: String::new(),
            tool_trace: Vec::new(),
            trace_writer: None,
            api_retries: 0,
            task_start: None,
            task_usage_base: 0,
            todo_mark: 0,
            session_stamp: None,
            settings,
            config,
            workspace_root,
            events,
            cancel,
            tool_ctx,
        }
    }

    // ------------------------------------------------------------ erişim

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn pending_mode(&self) -> Option<Mode> {
        self.pending_mode
    }

    pub fn auto_approve(&self) -> bool {
        self.auto_approve
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn messages(&self) -> &[Value] {
        &self.messages
    }

    pub fn task_goal(&self) -> &str {
        &self.task_goal
    }

    pub fn interrupted(&self) -> bool {
        self.interrupted
    }

    pub fn todos(&self) -> &TodoStore {
        &self.todos
    }

    pub fn vault_mut(&mut self) -> &mut SecretVault {
        &mut self.vault
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// `/steps <n>`: oturum adım tavanı (`None` → varsayılan).
    pub fn set_max_steps(&mut self, max_steps: Option<u32>) {
        self.max_steps = max_steps;
    }

    pub fn set_model(&mut self, new_model: &str) {
        self.model = new_model.to_string();
        self.sync_window();
    }

    pub(crate) fn sync_window(&mut self) {
        if self.window_model != self.model {
            self.window_model = self.model.clone();
            self.token_manager.context_window = context_window(&self.model);
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn set_auto_approve(&mut self, auto_approve: bool) {
        self.auto_approve = auto_approve;
    }

    /// Oturum geçmişini sıfırlar (`/clear`).
    pub fn clear_history(&mut self) {
        self.todos.clear();
        self.git_block = git_context_block(&self.workspace_root, GIT_BLOCK_TIMEOUT);
        self.system_prompt = compose_system(
            SYSTEM_PROMPT,
            &self.workspace_root,
            &self.config,
            &self.git_block,
        );
        self.messages = vec![json!({"role": "system", "content": self.system_prompt})];
        self.last_usage = None;
        self.pending_mode = None;
        self.interrupted = false;
        self.task_goal.clear();
        self.thinking = None;
        self.exec_profile = None;
        self.vault = SecretVault::new();
        self.settings = Settings::load(Some(&self.workspace_root));
    }

    // ------------------------------------------------------------ olaylar

    pub(crate) fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    pub(crate) fn info(&self, text: impl Into<String>) {
        self.emit(AgentEvent::Info(text.into()));
    }

    pub(crate) fn warning(&self, text: impl Into<String>) {
        self.emit(AgentEvent::Warning(text.into()));
    }

    pub(crate) fn error(&self, text: impl Into<String>) {
        self.emit(AgentEvent::Error(text.into()));
    }

    pub(crate) fn tool_result_event(
        &self,
        call_id: &str,
        name: &str,
        summary: &str,
        is_error: bool,
        detail: Option<&str>,
    ) {
        self.emit(AgentEvent::ToolResult {
            call_id: call_id.to_string(),
            name: name.to_string(),
            ok: !is_error,
            summary: summary.to_string(),
            detail: detail.unwrap_or(summary).to_string(),
        });
    }

    // ------------------------------------------------------------ geçmiş

    /// Modelin gönderdiği ama cevaplanmamış her araç çağrısına `note` ile cevap verir.
    pub(crate) fn close_open_tool_calls(&mut self, note: &str) {
        let answered: Vec<String> = self
            .messages
            .iter()
            .filter(|m| role(m) == "tool")
            .filter_map(|m| m.get("tool_call_id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let Some(last_assistant) = self.messages.iter().rev().find(|m| role(m) == "assistant")
        else {
            return;
        };
        let calls: Vec<Value> = last_assistant
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if calls.is_empty() {
            return;
        }
        for call in calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
            if answered.iter().any(|a| a == id) {
                continue;
            }
            let name = call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            self.messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "name": name,
                "content": note,
            }));
        }
    }

    /// Ctrl+C sonrası geçmişi kısa bir düzeltmeyle sürdürülebilir bırakır.
    pub fn note_interrupt(&mut self, mode: Mode) {
        self.close_open_tool_calls(
            "İPTAL: Kullanıcı Ctrl+C ile işlemi durdurdu. Çalışan komut sonlandırıldı. Geçmiş korunuyor; aynı komutu yeniden başlatma.",
        );
        self.messages.push(json!({
            "role": "assistant",
            "content": "Kullanıcı işlemi Ctrl+C ile durdurdu. Konuşma geçmişi ve tamamlanan adımlar duruyor. Kullanıcı küçük bir düzeltme ekleyebilir.",
        }));
        self.pending_mode = Some(mode);
        self.interrupted = true;
    }

    /// Modele giden araç çağrısı mesajı (`_tool_call_message`).
    pub(crate) fn tool_call_message(content: &str, tool_calls: &[Value]) -> Value {
        let calls: Vec<Value> = tool_calls
            .iter()
            .map(|tc| {
                let function = tc.get("function").cloned().unwrap_or(Value::Null);
                let arguments = match function.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) | None => "{}".to_string(),
                    Some(other) => other.to_string(),
                };
                json!({
                    "id": tc.get("id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": arguments,
                    },
                })
            })
            .collect();
        json!({"role": "assistant", "content": content, "tool_calls": calls})
    }

    pub(crate) fn tool_reply(&mut self, call_id: &str, fn_name: &str, content: &str) {
        self.messages.push(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "name": fn_name,
            "content": content,
        }));
    }

    /// Ham argümanları JSON nesnesine çevirir (`_parse_tool_args`).
    pub(crate) fn parse_tool_args(raw: Option<&Value>) -> Result<Value, String> {
        let parsed = match raw {
            Some(Value::Object(_)) => return Ok(raw.cloned().unwrap_or(Value::Null)),
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(Value::String(s)) if s.trim().is_empty() => Value::Object(Map::new()),
            Some(Value::String(s)) => {
                serde_json::from_str::<Value>(s).map_err(|e| e.to_string())?
            }
            Some(other) => other.clone(),
        };
        if parsed.is_object() {
            Ok(parsed)
        } else {
            Err(format!(
                "Araç argümanları bir JSON nesnesi olmalı, gelen tip: {}.",
                py_type_name(&parsed)
            ))
        }
    }

    /// Önceki görevlerin araç çıktılarını kırpar.
    pub(crate) fn trim_previous_tool_outputs(&mut self) {
        for m in self.messages.iter_mut() {
            if role(m) != "tool" {
                continue;
            }
            let Some(content) = m.get("content").and_then(Value::as_str) else {
                continue;
            };
            let trimmed = truncate_tool_content(content);
            if let Some(obj) = m.as_object_mut() {
                obj.insert("content".into(), Value::String(trimmed));
            }
        }
    }

    /// Görev sınırında önceki görevi tek deterministik özete indirir.
    pub(crate) fn collapse_previous_task(&mut self) {
        if !self.compactor.smart {
            return;
        }
        let used = self.token_manager.estimate_messages(&self.messages);
        if used <= self.compaction_limit() / 2 {
            return;
        }
        let mut start: Option<usize> = None;
        for i in (1..self.messages.len()).rev() {
            let m = &self.messages[i];
            let content = m.get("content").and_then(Value::as_str);
            if role(m) == "user"
                && content.is_some_and(|c| !TASK_NOISE_PREFIXES.iter().any(|p| c.starts_with(p)))
            {
                start = Some(i);
                break;
            }
        }
        let Some(start) = start else {
            return;
        };
        if start <= 1 {
            return;
        }
        let summary = crate::compaction::collapse_task(&self.messages[start..]);
        self.messages.truncate(start);
        self.messages
            .push(json!({"role": "user", "content": summary}));
    }

    // ------------------------------------------------------------ okuma tekrarı

    fn read_key(args: &Value) -> Option<String> {
        let path = args.get("path").and_then(Value::as_str)?;
        if path.is_empty() {
            return None;
        }
        let start = args.get("start_line").cloned().unwrap_or(Value::Null);
        let end = args.get("end_line").cloned().unwrap_or(Value::Null);
        Some(format!("{path}\u{0}{start}\u{0}{end}"))
    }

    fn range_text(args: &Value) -> String {
        let start = args.get("start_line").filter(|v| !v.is_null());
        let end = args.get("end_line").filter(|v| !v.is_null());
        if start.is_none() && end.is_none() {
            return "(ilk 250 satır)".to_string();
        }
        let start = start.map(py_scalar).unwrap_or_else(|| "1".into());
        let end = end.map(py_scalar).unwrap_or_else(|| "son".into());
        format!("{start}-{end}")
    }

    fn file_digest(&self, path: &str) -> Option<String> {
        let target = self.workspace_root.join(path);
        let bytes = std::fs::read(target).ok()?;
        let mut hasher = Sha1::new();
        hasher.update(&bytes);
        Some(format!("{:x}", hasher.finalize()))
    }

    /// Aynı dosya aralığı değişmeden yeniden okunuyorsa kısa not; yoksa boş.
    pub(crate) fn read_repeat(&self, fn_name: &str, args: &Value) -> String {
        if fn_name != "view_file" {
            return String::new();
        }
        let Some(key) = Self::read_key(args) else {
            return String::new();
        };
        let Some(record) = self.reads.get(&key) else {
            return String::new();
        };
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        match self.file_digest(path) {
            Some(d) if d == record.digest => {}
            _ => return String::new(),
        }
        for m in &self.messages {
            if role(m) != "tool"
                || m.get("tool_call_id").and_then(Value::as_str) != Some(record.call_id.as_str())
            {
                continue;
            }
            if let Some(content) = m.get("content").and_then(Value::as_str) {
                if !content.starts_with("[özet]")
                    && !content.starts_with("BİLGİ:")
                    && !content.contains("[araç çıktısı bağlam için kırpıldı]")
                {
                    return format!(
                        "BİLGİ: {path} {} adım {}'te okundu ve o zamandan beri değişmedi; yukarıdaki çıktıyı kullan.",
                        Self::range_text(args),
                        record.step
                    );
                }
            }
        }
        String::new()
    }

    pub(crate) fn note_read(&mut self, fn_name: &str, args: &Value, result: &str, call_id: &str) {
        if fn_name != "view_file"
            || result.starts_with("HATA")
            || result.starts_with("BİLGİ:")
            || result.starts_with("TEKRAR")
        {
            return;
        }
        let Some(key) = Self::read_key(args) else {
            return;
        };
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        if let Some(digest) = self.file_digest(path) {
            self.reads.insert(
                key,
                ReadRecord {
                    digest,
                    step: self.step_no,
                    call_id: call_id.to_string(),
                },
            );
        }
    }

    // ------------------------------------------------------------ izin kapısı

    /// `(kurala göre izinli, ret gerekçesi)`.
    pub(crate) fn permission_gate(&self, fn_name: &str, args: &Value) -> (bool, String) {
        let empty = Map::new();
        let args_map = args.as_object().unwrap_or(&empty);
        let verdict = self.settings.check_permission(fn_name, args_map);
        if verdict == Some(Permission::Deny) {
            return (
                false,
                "ayar dosyasındaki izin kuralı bu aracı engelliyor (permissions.deny).".into(),
            );
        }
        if let Some(reason) = self.settings.run_pre_tool(fn_name, args_map, &self.vault) {
            return (false, format!("pre_tool hook engelledi: {reason}"));
        }
        (verdict == Some(Permission::Allow), String::new())
    }

    pub(crate) fn run_post_tool(&self, fn_name: &str, args: &Value) {
        let empty = Map::new();
        self.settings
            .run_post_tool(fn_name, args.as_object().unwrap_or(&empty), &self.vault);
    }

    // ------------------------------------------------------------ kullanım

    /// Sunucu kullanım sayaçlarını saklar ve `usage.jsonl`'e yazar.
    pub(crate) fn record_usage(
        &mut self,
        usage: Option<&Map<String, Value>>,
        step: Option<u32>,
        model: Option<&str>,
        latency_ms: Option<i64>,
    ) {
        let Some(usage) = usage else {
            return;
        };
        let pick = |k: &str| usage.get(k).cloned().unwrap_or(Value::Null);
        let last = json!({
            "prompt_tokens": pick("prompt_tokens"),
            "completion_tokens": pick("completion_tokens"),
            "total_tokens": pick("total_tokens"),
        });
        let as_int = |v: &Value| v.as_i64();
        if let Some(n) = as_int(&last["prompt_tokens"]) {
            self.usage_totals.prompt_tokens += n;
        }
        if let Some(n) = as_int(&last["completion_tokens"]) {
            self.usage_totals.completion_tokens += n;
        }
        if let Some(n) = as_int(&last["total_tokens"]) {
            self.usage_totals.total_tokens += n;
        }
        self.usage_requests += 1;
        let usage_value = Value::Object(usage.clone());
        let model = model.unwrap_or(&self.model).to_string();
        log_usage(
            Some(&usage_value),
            &model,
            step.map(i64::from),
            self.mode.as_str(),
            &self.workspace_root,
            latency_ms,
        );
        self.emit(AgentEvent::Usage(last.clone()));
        self.last_usage = Some(last);
    }

    /// Sıkıştırmayı tetikleyen token tavanı.
    pub fn compaction_limit(&self) -> i64 {
        let window_limit =
            (f64::from(self.token_manager.context_window) * self.config.compaction_ratio()) as i64;
        self.config.context_budget().min(window_limit).max(1024)
    }

    /// `/tokens` raporu.
    pub fn token_report(&self) -> Value {
        let output_budget = self
            .token_manager
            .resolve_output_budget(&self.model, Some(self.config.default_max_tokens()));
        let estimated = self.token_manager.estimate_messages(&self.messages);
        json!({
            "messages": self.messages.len(),
            "estimated_prompt_tokens": estimated,
            "output_budget": output_budget,
            "context_window": self.token_manager.context_window,
            "compaction_limit": self.compaction_limit(),
            "compaction_ratio": self.config.compaction_ratio(),
            "context_budget": self.config.context_budget(),
            "remaining_context": self.token_manager.remaining_context(&self.messages, i64::from(output_budget)),
            "calibration": (self.token_manager.calibration() * 1000.0).round() / 1000.0,
            "last_usage": self.last_usage.clone().unwrap_or(Value::Null),
            "session_requests": self.usage_requests,
            "session_usage": {
                "prompt_tokens": self.usage_totals.prompt_tokens,
                "completion_tokens": self.usage_totals.completion_tokens,
                "total_tokens": self.usage_totals.total_tokens,
            },
        })
    }

    // ------------------------------------------------------------ değişiklik günlüğü

    pub(crate) fn note_tool_call(&mut self, name: &str, args: &Value, result: &str) {
        if self.tool_trace.len() >= 200 {
            return;
        }
        let ok = !(result.starts_with("HATA:")
            || result.starts_with("İPTAL:")
            || result.starts_with("REDDEDİLDİ:"));
        let mut record = ToolTraceRecord {
            name: name.to_string(),
            ok,
            ..Default::default()
        };
        if matches!(name, "edit_file" | "write_file") {
            record.path = args.get("path").and_then(Value::as_str).map(str::to_string);
        }
        if name == "run_command" {
            let command: String = args
                .get("command")
                .map(py_scalar)
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            let head: String = result.chars().take(40).collect();
            record.exit_ok = ok && head.contains("Çıkış Kodu: 0");
            record.summary = result
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(160)
                .collect();
            record.command = Some(command);
        }
        self.tool_trace.push(record);
    }

    /// Biten görevin değişiklik günlüğü girdisi.
    pub fn changelog_entry(&self, final_text: &str) -> Value {
        let mut files: Vec<String> = Vec::new();
        let mut commands: Vec<String> = Vec::new();
        let mut exit_codes: Vec<i64> = Vec::new();
        let mut verification = String::new();
        for record in &self.tool_trace {
            if let Some(path) = &record.path {
                if record.ok && !files.contains(path) {
                    files.push(path.clone());
                }
            }
            if let Some(command) = &record.command {
                if command.is_empty() {
                    continue;
                }
                if !commands.contains(command) {
                    commands.push(command.clone());
                }
                exit_codes.push(if record.exit_ok { 0 } else { 1 });
                if command.contains("pytest") && verification.is_empty() {
                    verification = format!("`{command}`");
                    if !record.summary.is_empty() {
                        verification.push_str(&format!(" -> {}", record.summary));
                    }
                }
            }
        }
        if verification.is_empty() {
            for m in self.messages.iter().rev() {
                if role(m) != "assistant" {
                    continue;
                }
                let Some(content) = m.get("content").and_then(Value::as_str) else {
                    continue;
                };
                if content.is_empty() {
                    continue;
                }
                verification = content
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect();
                break;
            }
        }
        let note: String = final_text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(200)
            .collect();
        json!({
            "goal": self.task_goal,
            "files": files,
            "commands": commands,
            "exit_codes": exit_codes,
            "verification": verification,
            "note": note,
        })
    }

    /// Görev sonu tek satır özeti (bir kez).
    pub(crate) fn print_task_summary(&mut self, steps: u32) {
        let Some(start) = self.task_start.take() else {
            return;
        };
        let seconds = start.elapsed().as_secs_f64();
        let tokens = (self.usage_totals.total_tokens - self.task_usage_base).max(0);
        self.emit(AgentEvent::RunSummary {
            steps,
            seconds,
            tokens,
        });
    }

    /// Yalnız bu görevde yazılmış listenin açık maddeleri.
    pub(crate) fn open_todos(&self) -> Vec<Value> {
        if self.todos.writes == self.todo_mark {
            return Vec::new();
        }
        self.todos
            .items()
            .iter()
            .filter(|t| t.status != "completed")
            .map(|t| json!({"content": t.content, "status": t.status}))
            .collect()
    }

    // ------------------------------------------------------------ yedek model

    /// Sağlayıcı aynı modeli art arda dolu/5xx ile reddediyorsa yedek modele geçer.
    pub(crate) fn maybe_fallback_model(&mut self, error: &EvrenError) -> bool {
        let fallback = self.config.fallback_model();
        if fallback.is_empty() || fallback == self.model {
            return false;
        }
        let status = error.status_code();
        let text = error.to_string().to_lowercase();
        let busy = matches!(status, Some(502 | 503 | 504 | 529))
            || ["dolu", "busy", "overloaded", "capacity", "unavailable"]
                .iter()
                .any(|k| text.contains(k));
        if !busy {
            return false;
        }
        if self.api_retries < self.config.fallback_after().max(1) {
            return false;
        }
        let previous = self.model.clone();
        self.set_model(&fallback);
        self.warning(format!(
            "Model {previous} art arda {} kez dolu/5xx döndü; görev yedek model {fallback} ile sürüyor (EVREN_FALLBACK_MODEL).",
            self.api_retries
        ));
        if let Some(writer) = &self.trace_writer {
            let mut record = Map::new();
            record.insert("type".into(), "model_fallback".into());
            record.insert("from".into(), previous.into());
            record.insert("to".into(), fallback.into());
            record.insert(
                "status".into(),
                status.map(Value::from).unwrap_or(Value::Null),
            );
            record.insert("retries".into(), self.api_retries.into());
            writer.record(record);
        }
        true
    }
}

/// Mesaj rolü (yoksa boş).
pub(crate) fn role(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or("")
}

/// Python `str(value)` benzeri kısa gösterim (dizeler tırnaksız).
pub(crate) fn py_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Araç bağlamı: onaylar ve çıktılar olay kanalına gider.
fn build_tool_context(
    root: &Path,
    config: &Config,
    events: EventSender,
    cancel: CancellationToken,
) -> ToolContext {
    let approve_events = events.clone();
    let approver: Approver = Box::new(move |req: &ApprovalRequest| {
        let (tx, rx) = oneshot::channel();
        if approve_events
            .send(AgentEvent::ApprovalRequest {
                request: req.clone(),
                reply: tx,
            })
            .is_err()
        {
            return false;
        }
        wait_blocking(&cancel, rx).unwrap_or(false)
    });
    let out_events = events.clone();
    let output: OutputSink = Box::new(move |stream, line| {
        let _ = out_events.send(AgentEvent::ToolOutput {
            stream,
            line: line.to_string(),
        });
    });
    let info: InfoSink = Box::new(move |text| {
        let _ = events.send(AgentEvent::Info(text.to_string()));
    });
    ToolContext::new(root, config)
        .with_approver(approver)
        .with_output(output)
        .with_info(info)
}

/// Engelleyici iş parçacığından (spawn_blocking) bir oneshot cevabını iptale duyarlı bekler.
pub(crate) fn wait_blocking<T: Send + 'static>(
    cancel: &CancellationToken,
    rx: oneshot::Receiver<T>,
) -> Option<T> {
    let cancel = cancel.clone();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(async move {
            tokio::select! {
                _ = cancel.cancelled() => None,
                r = rx => r.ok(),
            }
        }),
        Err(_) => rx.blocking_recv().ok(),
    }
}

/// Sıkıştırma özetleyicisi: ucuz modelle tek istek; her hatada `None`.
///
/// Senkron `Summarizer` imzası nedeniyle istek ayrı bir iş parçacığında
/// çalıştırılır ve çok iş parçacıklı Tokio çalışma zamanı gerektirir.
fn summarizer_for(client: Arc<ApiClient>, model: String) -> crate::compaction::Summarizer {
    Box::new(move |messages: &[Value]| {
        let request = crate::compaction::summary_request_messages(messages);
        if request.is_empty() {
            return None;
        }
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let client = client.clone();
        let model = model.clone();
        let fut = async move {
            let req = evren_api::ChatRequest {
                messages: request,
                model: Some(model),
                max_tokens: Some(crate::compaction::SUMMARY_MAX_TOKENS),
                temperature: Some(0.2),
                tools: None,
                tool_choice: None,
                max_retries: 1,
                thinking: Some(false),
            };
            let timeout = Duration::from_secs_f64(crate::compaction::SUMMARY_TIMEOUT_SECONDS);
            match tokio::time::timeout(timeout, client.chat_complete(req)).await {
                Ok(Ok(resp)) => resp.content().map(|t| t.trim().to_string()),
                _ => None,
            }
        };
        let text =
            std::thread::scope(|s| s.spawn(move || handle.block_on(fut)).join().ok().flatten())?;
        if text.is_empty() {
            return None;
        }
        Some(crate::compaction::clip_summary(&text))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_variants() {
        assert_eq!(
            AgentSession::parse_tool_args(Some(&json!({"a": 1}))).unwrap(),
            json!({"a": 1})
        );
        assert_eq!(AgentSession::parse_tool_args(None).unwrap(), json!({}));
        assert_eq!(
            AgentSession::parse_tool_args(Some(&json!("  "))).unwrap(),
            json!({})
        );
        assert_eq!(
            AgentSession::parse_tool_args(Some(&json!("{\"p\":\"x\"}"))).unwrap(),
            json!({"p": "x"})
        );
        let err = AgentSession::parse_tool_args(Some(&json!("[1]"))).unwrap_err();
        assert_eq!(
            err,
            "Araç argümanları bir JSON nesnesi olmalı, gelen tip: list."
        );
        assert!(AgentSession::parse_tool_args(Some(&json!("{bozuk"))).is_err());
    }

    #[test]
    fn tool_call_message_shape() {
        let calls = vec![
            json!({"id": "c1", "function": {"name": "view_file", "arguments": "{\"path\": \"a\"}"}}),
            json!({"id": "c2", "function": {"name": "run_command", "arguments": {"command": "ls"}}}),
            json!({"id": "c3", "function": {"name": "x"}}),
        ];
        let m = AgentSession::tool_call_message("", &calls);
        assert_eq!(m["role"], "assistant");
        assert_eq!(
            m["tool_calls"][0]["function"]["arguments"],
            "{\"path\": \"a\"}"
        );
        assert_eq!(m["tool_calls"][1]["type"], "function");
        assert_eq!(
            m["tool_calls"][1]["function"]["arguments"],
            "{\"command\":\"ls\"}"
        );
        assert_eq!(m["tool_calls"][2]["function"]["arguments"], "{}");
    }

    #[test]
    fn range_text_and_key() {
        assert_eq!(
            AgentSession::range_text(&json!({"path": "a"})),
            "(ilk 250 satır)"
        );
        assert_eq!(
            AgentSession::range_text(&json!({"path": "a", "start_line": 3})),
            "3-son"
        );
        assert_eq!(
            AgentSession::range_text(&json!({"path": "a", "end_line": 9})),
            "1-9"
        );
        assert!(AgentSession::read_key(&json!({"path": ""})).is_none());
        assert_ne!(
            AgentSession::read_key(&json!({"path": "a"})),
            AgentSession::read_key(&json!({"path": "a", "start_line": 1}))
        );
    }
}
