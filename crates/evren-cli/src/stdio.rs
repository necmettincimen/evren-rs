//! `--stdio` NDJSON v1 sunucusu (Python `stdio_server.py`).
//!
//! stdout yalnız protokol satırlarına ayrılır; ajan olayları `AgentEvent`
//! kanalından gelir ve Python `_on_event`/`_handle_approval` ile aynı zarf
//! biçimine çevrilir. Tur, ayrı bir tokio görevinde çalışır; tur bitince
//! `git status` farkı, oturum kaydı, `usage` ve `turn_end` gönderilir.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use evren_agent::mentions::expand_mentions;
use evren_agent::sessions::{list_sessions, session_summary, sessions_dir};
use evren_agent::{AgentEvent, AgentSession, EventSender};
use evren_api::ApiClient;
use evren_core::config::Config;
use evren_core::secret_vault::SecretVault;
use evren_core::settings::{add_allow_rule, rule_for, Args};
use evren_core::tools::{ApprovalReply, Mode};
use evren_core::{texts, EvrenError};
use evren_protocol::{
    CommandEnvelope, CommandType, ErrorCode, Event, EventType, IdCounter, PROTOCOL_VERSION,
};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// `tool_result.summary` için üst sınır (bayt); Python `_TOOL_RESULT_LIMIT`.
const TOOL_RESULT_LIMIT: usize = 4096;
/// `git status` beklemesi (Python `_git(timeout=30)`).
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// stdin kapanınca çalışan tura tanınan süre (Python `join(5)`).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Değişiklik listesinden düşülen ajan dosyaları (`_EVREN_FILES`).
const EVREN_FILES: [&str; 3] = ["agent.md", "changelog.md", "evren-changelog.md"];

/// `evren --stdio` seçenekleri (`run_stdio(args)`).
#[derive(Debug, Clone, Default)]
pub struct ServeOptions {
    pub model: Option<String>,
    pub yes: bool,
    pub allowed_tools: Vec<String>,
    pub continue_session: bool,
}

enum Pending {
    Approval(oneshot::Sender<ApprovalReply>),
    Ask(oneshot::Sender<Option<String>>),
}

struct Turn {
    cid: Option<String>,
    before: Vec<String>,
}

type TurnHandle = JoinHandle<(AgentSession, String)>;

struct Server {
    ids: IdCounter,
    workspace: PathBuf,
    config: Config,
    client: Arc<ApiClient>,
    opts: ServeOptions,
    /// Tur sırasında işçi göreve taşınır (`None` = meşgul).
    session: Option<AgentSession>,
    turn: Option<Turn>,
    turn_handle: Option<TurnHandle>,
    events_tx: EventSender,
    events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    pending: HashMap<String, Pending>,
    model: String,
    vault: Arc<Mutex<SecretVault>>,
    cancel: CancellationToken,
    /// Son `tool_call` (`always` kararı için kural üretimi).
    last_call: Option<(String, Args)>,
    /// Tur sırasında `/todos` ve `/tokens` için önbellek.
    todos: Vec<Value>,
    usage: (i64, i64),
    settings_dirty: bool,
    out: tokio::io::Stdout,
}

impl Server {
    fn new_session(&self) -> AgentSession {
        let mut session = AgentSession::new(
            self.client.clone(),
            self.config.clone(),
            &self.workspace,
            self.events_tx.clone(),
            CancellationToken::new(),
        );
        session.set_auto_approve(self.opts.yes);
        if !self.opts.allowed_tools.is_empty() {
            session.set_extra_allow(self.opts.allowed_tools.clone());
        }
        session.set_model(&self.model);
        session
    }

    /// Yeni (boş) oturumu etkin kılar; kasa ve önbellekler sıfırlanır.
    fn install_session(&mut self, session: AgentSession) {
        self.vault = session.vault();
        self.cancel = session.cancel_token();
        self.todos.clear();
        self.usage = (0, 0);
        self.session = Some(session);
    }

    fn busy(&self) -> bool {
        self.turn_handle.is_some()
    }

    fn mask(&self, text: &str) -> String {
        self.vault
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mask(text)
    }

    async fn send(&mut self, event: Event) {
        // stdout kapandıysa yapılacak bir şey yok; stdin EOF döngüyü bitirir.
        let _ = crate::write_line(&mut self.out, &event.to_line()).await;
    }

    async fn error(&mut self, cid: Option<&str>, code: ErrorCode, message: impl Into<String>) {
        let ev = Event::error(self.ids.event_id(), cid, code, message);
        self.send(ev).await;
    }

    fn event(&mut self, kind: EventType, cid: Option<&str>) -> Event {
        Event::new(kind, self.ids.event_id()).reply_to(cid)
    }

    async fn busy_error(&mut self, cid: Option<&str>) -> bool {
        if self.busy() {
            self.error(cid, ErrorCode::Busy, texts::STDIO_BUSY).await;
            return true;
        }
        false
    }

    // ------------------------------------------------------------ zarflar

    async fn handle_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                self.error(None, ErrorCode::InvalidJson, texts::stdio_invalid_json(&e))
                    .await;
                return;
            }
        };
        let is_valid = value.is_object() && value.get("type").is_some_and(Value::is_string);
        if !is_valid {
            self.error(
                None,
                ErrorCode::InvalidMessage,
                texts::STDIO_INVALID_MESSAGE,
            )
            .await;
            return;
        }
        let cid: Option<String> = value.get("id").and_then(Value::as_str).map(str::to_owned);
        let version_ok = match value.get("v") {
            None => true,
            Some(v) => v.as_u64() == Some(u64::from(PROTOCOL_VERSION)),
        };
        if !version_ok {
            self.error(
                cid.as_deref(),
                ErrorCode::UnsupportedVersion,
                texts::STDIO_UNSUPPORTED_VERSION,
            )
            .await;
            return;
        }
        let env: CommandEnvelope = match serde_json::from_value(value) {
            Ok(e) => e,
            Err(e) => {
                self.error(cid.as_deref(), ErrorCode::InvalidMessage, e.to_string())
                    .await;
                return;
            }
        };
        let Some(kind) = CommandType::parse(&env.kind) else {
            self.error(
                cid.as_deref(),
                ErrorCode::UnknownCommand,
                texts::stdio_unknown_command(&env.kind),
            )
            .await;
            return;
        };
        self.dispatch(kind, &env.data, cid.as_deref()).await;
    }

    async fn dispatch(&mut self, kind: CommandType, msg: &Map<String, Value>, cid: Option<&str>) {
        match kind {
            CommandType::Prompt => self.cmd_prompt(msg, cid).await,
            CommandType::Cancel => self.cmd_cancel(),
            CommandType::ApprovalResponse => self.cmd_approval_response(msg, cid).await,
            CommandType::AskUserResponse => self.cmd_ask_user_response(msg, cid).await,
            CommandType::SetModel => {
                let model = str_field(msg, "model");
                self.cmd_set_model(&model, cid).await;
            }
            CommandType::SetMode => {
                let mode = str_field(msg, "mode");
                self.cmd_set_mode(&mode, cid).await;
            }
            CommandType::ListSessions => {
                let limit = msg.get("limit").and_then(Value::as_i64).unwrap_or(20);
                self.cmd_list_sessions(limit, cid).await;
            }
            CommandType::Resume => {
                let sid = str_field(msg, "session_id");
                self.cmd_resume(&sid, cid).await;
            }
            CommandType::NewSession => self.cmd_new_session(cid).await,
            CommandType::ListModels => self.cmd_list_models(cid).await,
            CommandType::Quota => self.cmd_quota(cid).await,
            CommandType::Slash => self.cmd_slash(msg, cid).await,
        }
    }

    // ------------------------------------------------------------ komutlar

    async fn cmd_prompt(&mut self, msg: &Map<String, Value>, cid: Option<&str>) {
        if self.busy_error(cid).await {
            return;
        }
        let mut text = str_field(msg, "text");
        if text.trim().is_empty() {
            self.error(cid, ErrorCode::EmptyPrompt, texts::STDIO_EMPTY_PROMPT)
                .await;
            return;
        }
        if let Some(mode) = msg.get("mode").and_then(Value::as_str) {
            if mode == "ask" || mode == "plan" {
                text = format!("{mode}: {text}");
            }
        }
        let mentions: Vec<String> = msg
            .get("attachments")
            .and_then(Value::as_array)
            .map(|atts| atts.iter().filter_map(mention_from_attachment).collect())
            .unwrap_or_default();
        if !mentions.is_empty() {
            text = format!("{text}\n\n{}", mentions.join(" "));
        }
        self.start_turn(text, cid.map(str::to_owned)).await;
    }

    /// Python `_run_turn` başı: `turn_start`, `git status` anlık görüntüsü,
    /// bahis uyarıları, ardından işçi görevde `run_step`.
    async fn start_turn(&mut self, text: String, cid: Option<String>) {
        let Some(mut session) = self.session.take() else {
            return;
        };
        let ev = self.event(EventType::TurnStart, cid.as_deref());
        self.send(ev).await;
        let before = status_lines(&self.workspace).await;
        let (expanded, warnings) = expand_mentions(&text, &self.workspace);
        for w in warnings {
            self.error(cid.as_deref(), ErrorCode::Mention, w).await;
        }
        self.cancel = session.reset_cancel();
        self.turn = Some(Turn { cid, before });
        self.turn_handle = Some(tokio::spawn(async move {
            let result = session.run_step(&expanded, None, true).await;
            (session, result)
        }));
    }

    /// Python `_run_turn` sonu: değişen dosyalar, oturum kaydı, `usage`, `turn_end`.
    async fn finish_turn(
        &mut self,
        outcome: Result<(AgentSession, String), tokio::task::JoinError>,
    ) {
        self.turn_handle = None;
        // İşçinin kapanmadan önce gönderdiği olaylar sırayla işlenir.
        while let Ok(ev) = self.events_rx.try_recv() {
            self.on_event(ev).await;
        }
        self.pending.clear();
        let Some(turn) = self.turn.take() else {
            return;
        };
        let cid = turn.cid.as_deref();
        let (session, result) = match outcome {
            Ok((session, result)) => (Some(session), result),
            Err(e) => (None, format!("HATA: {e}")),
        };
        let code = u8::from(result.starts_with("HATA:") || result.starts_with("İPTAL:"));
        let after = status_lines(&self.workspace).await;
        let changed: Vec<Value> = after
            .iter()
            .filter(|ln| !turn.before.contains(ln))
            .map(|ln| status_path(ln))
            .filter(|p| !is_evren_file(p))
            .map(Value::from)
            .collect();
        let mut session = match session {
            Some(s) => s,
            None => self.new_session(),
        };
        session.save_session();
        if self.settings_dirty {
            session.reload_settings();
            self.settings_dirty = false;
        }
        self.usage = session_usage(&session);
        self.session = Some(session);
        self.send_usage(cid).await;
        let ev = self
            .event(EventType::TurnEnd, cid)
            .field("result", result)
            .field("changed_files", Value::Array(changed))
            .field("exit_code", code);
        self.send(ev).await;
    }

    async fn send_usage(&mut self, cid: Option<&str>) {
        let (p, c) = self.usage;
        let ev = self
            .event(EventType::Usage, cid)
            .field("prompt_tokens", p)
            .field("completion_tokens", c);
        self.send(ev).await;
    }

    fn cmd_cancel(&mut self) {
        if !self.busy() {
            return;
        }
        self.cancel.cancel();
        // Bekleyen onay/sorular düşer: onay reddedilir, soru cevapsız kalır.
        self.pending.clear();
    }

    async fn cmd_approval_response(&mut self, msg: &Map<String, Value>, cid: Option<&str>) {
        let decision = msg.get("decision").and_then(Value::as_str).unwrap_or("");
        if !matches!(decision, "allow" | "deny" | "always") {
            self.error(
                cid,
                ErrorCode::InvalidDecision,
                texts::STDIO_INVALID_DECISION,
            )
            .await;
            return;
        }
        let rid = request_id_of(msg);
        let Some(Pending::Approval(tx)) = self.pending.remove(&rid) else {
            self.error(
                cid,
                ErrorCode::UnknownRequest,
                texts::STDIO_UNKNOWN_APPROVAL,
            )
            .await;
            return;
        };
        if decision == "always" {
            self.remember_current();
        }
        let reply = ApprovalReply {
            approved: decision != "deny",
            edited_content: msg
                .get("edited_content")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        let _ = tx.send(reply);
    }

    /// `always`: son araç çağrısı için kalıcı izin kuralı (`settings.local.json`).
    fn remember_current(&mut self) {
        let Some((name, args)) = &self.last_call else {
            return;
        };
        let rule = rule_for(name, args);
        if add_allow_rule(&self.workspace, &rule).is_ok() {
            self.settings_dirty = true;
        }
    }

    async fn cmd_ask_user_response(&mut self, msg: &Map<String, Value>, cid: Option<&str>) {
        let rid = request_id_of(msg);
        let Some(Pending::Ask(tx)) = self.pending.remove(&rid) else {
            self.error(
                cid,
                ErrorCode::UnknownRequest,
                texts::STDIO_UNKNOWN_QUESTION,
            )
            .await;
            return;
        };
        let answer = msg.get("answer").and_then(Value::as_str).map(str::to_owned);
        let _ = tx.send(answer);
    }

    async fn cmd_set_model(&mut self, model: &str, cid: Option<&str>) {
        if self.busy_error(cid).await {
            return;
        }
        let model = model.trim();
        if model.is_empty() {
            self.error(cid, ErrorCode::InvalidModel, texts::STDIO_MODEL_EMPTY)
                .await;
            return;
        }
        if let Some(session) = self.session.as_mut() {
            session.set_model(model);
        }
        self.model = model.to_string();
        let ev = self
            .event(EventType::ModelChanged, cid)
            .field("model", model);
        self.send(ev).await;
    }

    async fn cmd_set_mode(&mut self, mode: &str, cid: Option<&str>) {
        if self.busy_error(cid).await {
            return;
        }
        let Some(parsed) = Mode::parse(mode) else {
            self.error(cid, ErrorCode::InvalidMode, texts::STDIO_INVALID_MODE)
                .await;
            return;
        };
        if let Some(session) = self.session.as_mut() {
            session.set_mode(parsed);
        }
        let ev = self
            .event(EventType::ModeChanged, cid)
            .field("mode", parsed.as_str());
        self.send(ev).await;
    }

    async fn cmd_list_sessions(&mut self, limit: i64, cid: Option<&str>) {
        let limit = usize::try_from(limit).unwrap_or(0);
        let items: Vec<Value> = list_sessions(&self.workspace, limit)
            .iter()
            .map(|p| session_summary(p))
            .collect();
        let ev = self
            .event(EventType::Sessions, cid)
            .field("items", Value::Array(items));
        self.send(ev).await;
    }

    async fn cmd_resume(&mut self, sid: &str, cid: Option<&str>) {
        if self.busy_error(cid).await {
            return;
        }
        let path = sessions_dir(&self.workspace).join(format!("{sid}.json"));
        if sid.is_empty() || sid.contains('/') || sid.contains('\\') || !path.is_file() {
            self.error(
                cid,
                ErrorCode::UnknownSession,
                texts::stdio_unknown_session(sid),
            )
            .await;
            return;
        }
        let mut session = self.new_session();
        if !session.load_session(&path) {
            self.install_session(session);
            self.error(
                cid,
                ErrorCode::UnknownSession,
                texts::stdio_session_unreadable(sid),
            )
            .await;
            return;
        }
        let messages = history_of(session.messages());
        self.install_session(session);
        let ev = self
            .event(EventType::SessionLoaded, cid)
            .field("session_id", sid)
            .field("messages", Value::Array(messages));
        self.send(ev).await;
    }

    async fn cmd_new_session(&mut self, cid: Option<&str>) {
        if self.busy_error(cid).await {
            return;
        }
        let session = self.new_session();
        self.install_session(session);
        let ev = self
            .event(EventType::SessionLoaded, cid)
            .field("session_id", Value::Null)
            .field("messages", Value::Array(Vec::new()));
        self.send(ev).await;
    }

    async fn cmd_list_models(&mut self, cid: Option<&str>) {
        let list: Vec<Value> = self
            .client
            .list_models()
            .await
            .into_iter()
            .map(Value::from)
            .collect();
        let ev = self
            .event(EventType::Models, cid)
            .field("models", Value::Array(list))
            .field("current", self.model.as_str());
        self.send(ev).await;
    }

    async fn cmd_quota(&mut self, cid: Option<&str>) {
        let quota = self.client.get_quota(None).await;
        let masked = self.mask_value(&Value::Object(quota));
        let ev = self.event(EventType::Quota, cid).field("quota", masked);
        self.send(ev).await;
    }

    /// Python `_mask(dict)`: tüm dize değerleri kasa ile maskelenir.
    fn mask_value(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.mask(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.mask_value(x)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| (k.clone(), self.mask_value(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    async fn cmd_slash(&mut self, msg: &Map<String, Value>, cid: Option<&str>) {
        let command = str_field(msg, "command");
        let command = command.trim_start_matches('/').to_lowercase();
        let args = str_field(msg, "args");
        let args = args.trim().to_string();
        match command.as_str() {
            "model" if !args.is_empty() => self.cmd_set_model(&args, cid).await,
            "model" | "models" => self.cmd_list_models(cid).await,
            "mode" => self.cmd_set_mode(&args, cid).await,
            "new" | "clear" => self.cmd_new_session(cid).await,
            "resume" if !args.is_empty() => self.cmd_resume(&args, cid).await,
            "resume" | "sessions" => self.cmd_list_sessions(20, cid).await,
            "quota" => self.cmd_quota(cid).await,
            "tokens" => {
                if let Some(session) = &self.session {
                    self.usage = session_usage(session);
                }
                self.send_usage(cid).await;
            }
            "todos" => {
                if let Some(session) = &self.session {
                    self.todos = session.todos().snapshot();
                }
                let items = Value::Array(self.todos.clone());
                let ev = self.event(EventType::Todos, cid).field("items", items);
                self.send(ev).await;
            }
            _ => {
                self.error(
                    cid,
                    ErrorCode::UnsupportedSlash,
                    texts::stdio_unsupported_slash(&command),
                )
                .await;
            }
        }
    }

    // ------------------------------------------------------------- olaylar

    /// Python `_on_event` + `_handle_approval`: yalnız protokol olayları yazılır.
    async fn on_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::AssistantDelta(text) => {
                let ev = self
                    .event(EventType::AssistantDelta, None)
                    .field("text", text);
                self.send(ev).await;
            }
            AgentEvent::ReasoningDelta(text) => {
                let ev = self
                    .event(EventType::ReasoningDelta, None)
                    .field("text", text);
                self.send(ev).await;
            }
            AgentEvent::ToolCall {
                call_id,
                name,
                args,
            } => {
                let call_id = if call_id.is_empty() {
                    self.ids.call_id()
                } else {
                    call_id
                };
                let arg_map = match &args {
                    Value::Object(m) => m.clone(),
                    _ => Map::new(),
                };
                self.last_call = Some((name.clone(), arg_map));
                let ev = self
                    .event(EventType::ToolCall, None)
                    .field("call_id", call_id)
                    .field("name", name)
                    .field("args", args);
                self.send(ev).await;
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                ok,
                detail,
                ..
            } => {
                let (clipped, truncated) = clip_utf8(&detail, TOOL_RESULT_LIMIT);
                let summary = self.mask(clipped);
                let ev = self
                    .event(EventType::ToolResult, None)
                    .field("call_id", call_id)
                    .field("name", name)
                    .field("ok", ok)
                    .field("summary", summary)
                    .field("output_truncated", truncated);
                self.send(ev).await;
            }
            AgentEvent::ApprovalRequest { request, reply } => {
                let rid = self.ids.request_id();
                self.pending.insert(rid.clone(), Pending::Approval(reply));
                if let Some(p) = &request.proposal {
                    // Dosya içerikleri maskelenmez: düzenlenmiş kopya olduğu gibi yazılır.
                    let ev = self
                        .event(EventType::FileChangeProposed, None)
                        .field("request_id", rid.as_str())
                        .field("path", p.path.as_str())
                        .field("old_content", p.old.as_str())
                        .field("new_content", p.new.as_str());
                    self.send(ev).await;
                }
                let title = self.mask(&request.title);
                let detail = self.mask(&request.detail);
                let ev = self
                    .event(EventType::ApprovalRequest, None)
                    .field("request_id", rid)
                    .field("kind", request.ui_kind)
                    .field("title", title)
                    .field("detail", detail);
                self.send(ev).await;
            }
            AgentEvent::AskUser {
                question,
                kind,
                reply,
            } => {
                let rid = self.ids.request_id();
                self.pending.insert(rid.clone(), Pending::Ask(reply));
                let options = if kind == "text" {
                    Value::Null
                } else {
                    json!(["evet", "hayır"])
                };
                let ev = self
                    .event(EventType::AskUser, None)
                    .field("request_id", rid)
                    .field("question", question)
                    .field("options", options);
                self.send(ev).await;
            }
            AgentEvent::Todos(items) => {
                self.todos = items.clone();
                let ev = self
                    .event(EventType::Todos, None)
                    .field("items", Value::Array(items));
                self.send(ev).await;
            }
            // Spinner, bilgi, uyarı, tam metinler, canlı çıktı ve özetler
            // protokole yazılmaz (Python sink yalnız beş olayı dinler).
            AgentEvent::Status(_)
            | AgentEvent::Info(_)
            | AgentEvent::Warning(_)
            | AgentEvent::Error(_)
            | AgentEvent::Reasoning(_)
            | AgentEvent::Assistant(_)
            | AgentEvent::ToolOutput { .. }
            | AgentEvent::Usage(_)
            | AgentEvent::RunSummary { .. } => {}
        }
    }
}

// ------------------------------------------------------------------ yardımcılar

fn str_field(msg: &Map<String, Value>, key: &str) -> String {
    match msg.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn request_id_of(msg: &Map<String, Value>) -> String {
    match msg.get("request_id") {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Python `_mention(att)`: `@yol` ya da `@yol:başlangıç-bitiş`.
fn mention_from_attachment(att: &Value) -> Option<String> {
    let obj = att.as_object()?;
    let path = match obj.get("path") {
        Some(Value::String(s)) if !s.is_empty() => s.replace('\\', "/"),
        _ => return None,
    };
    let range = obj.get("range").and_then(Value::as_object);
    let start = range
        .and_then(|r| r.get("start"))
        .and_then(as_int)
        .filter(|&n| n != 0);
    match start {
        Some(start) => {
            let end = range
                .and_then(|r| r.get("end"))
                .and_then(as_int)
                .filter(|&n| n != 0)
                .unwrap_or(start);
            Some(format!("@{path}:{start}-{end}"))
        }
        None => Some(format!("@{path}")),
    }
}

fn as_int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

/// Python `_is_evren_file`.
pub(crate) fn is_evren_file(path: &str) -> bool {
    let p = path.trim_matches('"').replace('\\', "/");
    p.starts_with(".evren/") || EVREN_FILES.contains(&p.as_str())
}

/// `git status --short` satırından yol: `ln[3:].strip()`.
pub(crate) fn status_path(line: &str) -> String {
    line.chars().skip(3).collect::<String>().trim().to_string()
}

/// Python `_status_lines`: `git status --short --untracked-files=all` satırları;
/// hata durumunda stdout+stderr (Python `_git` dönüş kodunu yoksayar), çalışmazsa boş.
pub(crate) async fn status_lines(workspace: &Path) -> Vec<String> {
    let run = tokio::process::Command::new("git")
        .args(["status", "--short", "--untracked-files=all"])
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .output();
    let out = match tokio::time::timeout(GIT_TIMEOUT, run).await {
        Ok(Ok(out)) => out,
        _ => return Vec::new(),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Python `_history`: yeniden çizim için kullanıcı/asistan metin turları.
fn history_of(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|m| {
            let role = m.get("role").and_then(Value::as_str)?;
            if role != "user" && role != "assistant" {
                return None;
            }
            let content = m.get("content").and_then(Value::as_str)?;
            if content.trim().is_empty() {
                return None;
            }
            Some(json!({"role": role, "text": content}))
        })
        .collect()
}

/// `token_report()["session_usage"]` → (prompt_tokens, completion_tokens).
fn session_usage(session: &AgentSession) -> (i64, i64) {
    let report = session.token_report();
    let usage = report.get("session_usage");
    let get = |k: &str| {
        usage
            .and_then(|u| u.get(k))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    (get("prompt_tokens"), get("completion_tokens"))
}

/// İlk `limit` baytı karakter sınırında keser (Python `errors="ignore"`).
fn clip_utf8(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

async fn wait_turn(
    handle: &mut Option<TurnHandle>,
) -> Result<(AgentSession, String), tokio::task::JoinError> {
    match handle {
        Some(h) => h.await,
        None => std::future::pending().await,
    }
}

// ------------------------------------------------------------------- giriş

/// `evren --stdio`: `ready` yazar, stdin satırlarını işler, EOF'ta turu
/// iptal edip (en çok 5 sn) çıkar. Dönüş kodu her zaman 0'dır.
pub async fn serve(
    config: &Config,
    opts: ServeOptions,
    workspace: &Path,
) -> Result<(), EvrenError> {
    let client = Arc::new(ApiClient::new(config)?);
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let model = opts.model.clone().unwrap_or_else(|| config.default_model());
    let mut server = Server {
        ids: IdCounter::new(),
        workspace: workspace.to_path_buf(),
        config: config.clone(),
        client,
        opts,
        session: None,
        turn: None,
        turn_handle: None,
        events_tx,
        events_rx,
        pending: HashMap::new(),
        model,
        vault: Arc::new(Mutex::new(SecretVault::new())),
        cancel: CancellationToken::new(),
        last_call: None,
        todos: Vec::new(),
        usage: (0, 0),
        settings_dirty: false,
        out: tokio::io::stdout(),
    };
    let session = server.new_session();
    server.install_session(session);
    if server.opts.continue_session {
        if let Some(last) = list_sessions(workspace, 1).first() {
            if let Some(session) = server.session.as_mut() {
                session.load_session(last);
            }
        }
    }

    let ready = Event::ready(
        server.ids.event_id(),
        evren_core::VERSION,
        Some(&server.model),
        &workspace.to_string_lossy(),
    );
    server.send(ready).await;

    let mut lines = crate::stdin_lines();
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => server.handle_line(&line).await,
                _ => break,
            },
            Some(ev) = server.events_rx.recv() => server.on_event(ev).await,
            outcome = wait_turn(&mut server.turn_handle) => server.finish_turn(outcome).await,
        }
    }

    // stdin kapandı: çalışan tur iptal edilir, kısa süre beklenir.
    server.cmd_cancel();
    if server.turn_handle.is_some() {
        let deadline = tokio::time::sleep(SHUTDOWN_GRACE);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                () = &mut deadline => break,
                Some(ev) = server.events_rx.recv() => server.on_event(ev).await,
                outcome = wait_turn(&mut server.turn_handle) => {
                    server.finish_turn(outcome).await;
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn mention_formats() {
        let att = json!({"path": "src\\a.py", "range": {"start": 3, "end": 7}});
        assert_eq!(mention_from_attachment(&att).unwrap(), "@src/a.py:3-7");
        let att = json!({"path": "a.py", "range": {"start": 3}});
        assert_eq!(mention_from_attachment(&att).unwrap(), "@a.py:3-3");
        let att = json!({"path": "a.py", "range": {"start": 0, "end": 7}});
        assert_eq!(mention_from_attachment(&att).unwrap(), "@a.py");
        assert!(mention_from_attachment(&json!({"path": ""})).is_none());
        assert!(mention_from_attachment(&json!("a.py")).is_none());
    }

    #[test]
    fn evren_files_and_status_paths() {
        assert!(is_evren_file(".evren/sessions/x.json"));
        assert!(is_evren_file("\"agent.md\""));
        assert!(is_evren_file(".evren\\todo.json"));
        assert!(!is_evren_file("src/agent.md"));
        assert_eq!(status_path("?? notlar.txt"), "notlar.txt");
        assert_eq!(status_path(" M \"a b.txt\""), "\"a b.txt\"");
    }

    #[test]
    fn clip_at_char_boundary() {
        let s = "aç".repeat(3000); // 9000 bayt
        let (c, t) = clip_utf8(&s, TOOL_RESULT_LIMIT);
        assert!(t);
        assert!(c.len() <= TOOL_RESULT_LIMIT);
        assert!(c.len() >= TOOL_RESULT_LIMIT - 3);
        let (c, t) = clip_utf8("kısa", 100);
        assert_eq!((c, t), ("kısa", false));
    }

    #[test]
    fn history_filters_text_turns() {
        let msgs = vec![
            json!({"role": "system", "content": "s"}),
            json!({"role": "user", "content": "merhaba"}),
            json!({"role": "assistant", "content": "   "}),
            json!({"role": "assistant", "content": null, "tool_calls": []}),
            json!({"role": "tool", "content": "x"}),
            json!({"role": "assistant", "content": "selam"}),
        ];
        assert_eq!(
            history_of(&msgs),
            vec![
                json!({"role": "user", "text": "merhaba"}),
                json!({"role": "assistant", "text": "selam"}),
            ]
        );
    }

    #[test]
    fn string_fields() {
        let m = json!({"a": "x", "b": 5, "c": null})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(str_field(&m, "a"), "x");
        assert_eq!(str_field(&m, "b"), "5");
        assert_eq!(str_field(&m, "c"), "");
        assert_eq!(str_field(&m, "d"), "");
        assert_eq!(request_id_of(&m), "None");
    }
}
