//! `evren --stdio` NDJSON protokolü, sürüm 1.
//!
//! Sözleşme `protocol/stdio-v1.schema.json` dosyasıdır; buradaki enum'lar
//! şemadaki `x-command-types`, `x-event-types` ve `x-error-codes` listeleriyle
//! testte eşitlenir.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Desteklenen protokol sürümü (`v` alanı).
pub const PROTOCOL_VERSION: u32 = 1;

/// Şema dosyasının `x-min-cli-version` değeri.
pub const MIN_CLI_VERSION: &str = "3.10.0";

/// `error` olayındaki `code` alanı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidJson,
    InvalidMessage,
    UnsupportedVersion,
    UnknownCommand,
    Busy,
    EmptyPrompt,
    InvalidDecision,
    UnknownRequest,
    InvalidMode,
    InvalidModel,
    UnknownSession,
    UnsupportedSlash,
    Mention,
    Quota,
    Internal,
    BadRole,
    BadRoot,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 17] = [
        ErrorCode::InvalidJson,
        ErrorCode::InvalidMessage,
        ErrorCode::UnsupportedVersion,
        ErrorCode::UnknownCommand,
        ErrorCode::Busy,
        ErrorCode::EmptyPrompt,
        ErrorCode::InvalidDecision,
        ErrorCode::UnknownRequest,
        ErrorCode::InvalidMode,
        ErrorCode::InvalidModel,
        ErrorCode::UnknownSession,
        ErrorCode::UnsupportedSlash,
        ErrorCode::Mention,
        ErrorCode::Quota,
        ErrorCode::Internal,
        ErrorCode::BadRole,
        ErrorCode::BadRoot,
    ];

    /// Şemadaki dizge karşılığı (`snake_case`).
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidJson => "invalid_json",
            ErrorCode::InvalidMessage => "invalid_message",
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::UnknownCommand => "unknown_command",
            ErrorCode::Busy => "busy",
            ErrorCode::EmptyPrompt => "empty_prompt",
            ErrorCode::InvalidDecision => "invalid_decision",
            ErrorCode::UnknownRequest => "unknown_request",
            ErrorCode::InvalidMode => "invalid_mode",
            ErrorCode::InvalidModel => "invalid_model",
            ErrorCode::UnknownSession => "unknown_session",
            ErrorCode::UnsupportedSlash => "unsupported_slash",
            ErrorCode::Mention => "mention",
            ErrorCode::Quota => "quota",
            ErrorCode::Internal => "internal",
            ErrorCode::BadRole => "bad_role",
            ErrorCode::BadRoot => "bad_root",
        }
    }
}

/// İstemciden gelen komut türleri.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandType {
    Prompt,
    Cancel,
    ApprovalResponse,
    AskUserResponse,
    SetModel,
    SetMode,
    ListSessions,
    Resume,
    NewSession,
    ListModels,
    Quota,
    Slash,
}

impl CommandType {
    pub const ALL: [CommandType; 12] = [
        CommandType::Prompt,
        CommandType::Cancel,
        CommandType::ApprovalResponse,
        CommandType::AskUserResponse,
        CommandType::SetModel,
        CommandType::SetMode,
        CommandType::ListSessions,
        CommandType::Resume,
        CommandType::NewSession,
        CommandType::ListModels,
        CommandType::Quota,
        CommandType::Slash,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CommandType::Prompt => "prompt",
            CommandType::Cancel => "cancel",
            CommandType::ApprovalResponse => "approval_response",
            CommandType::AskUserResponse => "ask_user_response",
            CommandType::SetModel => "set_model",
            CommandType::SetMode => "set_mode",
            CommandType::ListSessions => "list_sessions",
            CommandType::Resume => "resume",
            CommandType::NewSession => "new_session",
            CommandType::ListModels => "list_models",
            CommandType::Quota => "quota",
            CommandType::Slash => "slash",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

/// CLI'nın yayınladığı olay türleri.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Ready,
    TurnStart,
    AssistantDelta,
    ReasoningDelta,
    ToolCall,
    ToolResult,
    FileChangeProposed,
    ApprovalRequest,
    AskUser,
    Todos,
    Usage,
    TurnEnd,
    Error,
    ModelChanged,
    ModeChanged,
    Sessions,
    SessionLoaded,
    Models,
    Quota,
}

impl EventType {
    pub const ALL: [EventType; 19] = [
        EventType::Ready,
        EventType::TurnStart,
        EventType::AssistantDelta,
        EventType::ReasoningDelta,
        EventType::ToolCall,
        EventType::ToolResult,
        EventType::FileChangeProposed,
        EventType::ApprovalRequest,
        EventType::AskUser,
        EventType::Todos,
        EventType::Usage,
        EventType::TurnEnd,
        EventType::Error,
        EventType::ModelChanged,
        EventType::ModeChanged,
        EventType::Sessions,
        EventType::SessionLoaded,
        EventType::Models,
        EventType::Quota,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventType::Ready => "ready",
            EventType::TurnStart => "turn_start",
            EventType::AssistantDelta => "assistant_delta",
            EventType::ReasoningDelta => "reasoning_delta",
            EventType::ToolCall => "tool_call",
            EventType::ToolResult => "tool_result",
            EventType::FileChangeProposed => "file_change_proposed",
            EventType::ApprovalRequest => "approval_request",
            EventType::AskUser => "ask_user",
            EventType::Todos => "todos",
            EventType::Usage => "usage",
            EventType::TurnEnd => "turn_end",
            EventType::Error => "error",
            EventType::ModelChanged => "model_changed",
            EventType::ModeChanged => "mode_changed",
            EventType::Sessions => "sessions",
            EventType::SessionLoaded => "session_loaded",
            EventType::Models => "models",
            EventType::Quota => "quota",
        }
    }
}

/// Gelen komut zarfı: `type` zorunlu, `v` yoksa 1 sayılır, `id` isteğe
/// bağlı; diğer alanlar `data` içinde kalır.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEnvelope {
    #[serde(default = "default_version")]
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub data: Map<String, Value>,
}

fn default_version() -> u32 {
    PROTOCOL_VERSION
}

/// Giden olay zarfı. Alan sırası Python çıktısıyla aynıdır:
/// `v`, `type`, `id`, (`reply_to`), ardından olay verisi.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: EventType,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(flatten)]
    pub data: Map<String, Value>,
}

impl Event {
    pub fn new(kind: EventType, id: String) -> Self {
        Event {
            v: PROTOCOL_VERSION,
            kind,
            id,
            reply_to: None,
            data: Map::new(),
        }
    }

    pub fn reply_to(mut self, cid: Option<&str>) -> Self {
        self.reply_to = cid.map(str::to_owned);
        self
    }

    pub fn field(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.data.insert(key.to_owned(), value.into());
        self
    }

    /// `ready` olayı: süreç başlayınca bir kez.
    pub fn ready(id: String, version: &str, model: Option<&str>, workspace: &str) -> Self {
        Event::new(EventType::Ready, id)
            .field("version", version)
            .field("model", model.map(Value::from).unwrap_or(Value::Null))
            .field("workspace", workspace)
            .field("protocol", PROTOCOL_VERSION)
    }

    /// `error` olayı.
    pub fn error(
        id: String,
        cid: Option<&str>,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Event::new(EventType::Error, id)
            .reply_to(cid)
            .field("message", message.into())
            .field("code", code.as_str())
    }

    /// Tek satır NDJSON (sonunda `\n` yok). Serileştirme hatası pratikte
    /// olmaz; olursa `error/internal` döner.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            format!(
                "{{\"v\":{},\"type\":\"error\",\"id\":{:?},\"message\":{:?},\"code\":\"internal\"}}",
                PROTOCOL_VERSION,
                self.id,
                e.to_string()
            )
        })
    }
}

/// Protokol `id` üreticisi: `s1`, `s2`, ... (olaylar) ve `r`/`c` önekli
/// istek/çağrı kimlikleri aynı sayaçtan beslenir (Python ile aynı).
#[derive(Debug, Default)]
pub struct IdCounter(u64);

impl IdCounter {
    pub fn new() -> Self {
        IdCounter(0)
    }
    fn next(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
    pub fn event_id(&mut self) -> String {
        format!("s{}", self.next())
    }
    pub fn request_id(&mut self) -> String {
        format!("r{}", self.next())
    }
    pub fn call_id(&mut self) -> String {
        format!("c{}", self.next())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SCHEMA: &str = include_str!("../../../protocol/stdio-v1.schema.json");

    fn schema() -> Value {
        serde_json::from_str(SCHEMA).unwrap()
    }

    fn string_list(v: &Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn error_codes_match_schema() {
        let s = schema();
        let want = string_list(&s["x-error-codes"]);
        let have: Vec<String> = ErrorCode::ALL
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect();
        assert_eq!(have, want);
        for code in ErrorCode::ALL {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }

    #[test]
    fn command_and_event_types_match_schema() {
        let s = schema();
        let cmds: Vec<String> = CommandType::ALL
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect();
        assert_eq!(cmds, string_list(&s["x-command-types"]));
        let evts: Vec<String> = EventType::ALL
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect();
        assert_eq!(evts, string_list(&s["x-event-types"]));
        assert_eq!(
            s["x-protocol-version"].as_u64().unwrap(),
            u64::from(PROTOCOL_VERSION)
        );
        assert_eq!(s["x-min-cli-version"].as_str().unwrap(), MIN_CLI_VERSION);
    }

    #[test]
    fn ready_event_shape() {
        let mut ids = IdCounter::new();
        let ev = Event::ready(ids.event_id(), "4.0.0", Some("m"), "D:\\ws");
        let line = ev.to_line();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["type"], "ready");
        assert_eq!(v["id"], "s1");
        assert_eq!(v["version"], "4.0.0");
        assert_eq!(v["model"], "m");
        assert_eq!(v["workspace"], "D:\\ws");
        assert_eq!(v["protocol"], 1);
        assert!(line.starts_with("{\"v\":1,\"type\":\"ready\",\"id\":\"s1\",\"version\""));
        assert!(!line.contains("reply_to"));
    }

    #[test]
    fn error_event_with_reply_to() {
        let ev = Event::error(
            "s2".into(),
            Some("c1"),
            ErrorCode::UnknownCommand,
            "Bilinmeyen komut: x",
        );
        let v: Value = serde_json::from_str(&ev.to_line()).unwrap();
        assert_eq!(v["reply_to"], "c1");
        assert_eq!(v["code"], "unknown_command");
        assert_eq!(v["message"], "Bilinmeyen komut: x");
    }

    #[test]
    fn command_envelope_ignores_unknown_fields_and_defaults_version() {
        let c: CommandEnvelope =
            serde_json::from_str(r#"{"type":"prompt","text":"hi","garbage":1}"#).unwrap();
        assert_eq!(c.v, 1);
        assert_eq!(c.kind, "prompt");
        assert!(c.id.is_none());
        assert_eq!(c.data["text"], "hi");
        assert_eq!(CommandType::parse(&c.kind), Some(CommandType::Prompt));
        assert_eq!(CommandType::parse("yok"), None);
    }

    #[test]
    fn id_counter_is_shared() {
        let mut ids = IdCounter::new();
        assert_eq!(ids.event_id(), "s1");
        assert_eq!(ids.request_id(), "r2");
        assert_eq!(ids.call_id(), "c3");
        assert_eq!(ids.event_id(), "s4");
    }
}
