//! `--stdio` NDJSON v1 sunucusu. `handle_line` zarf denetimleri Python
//! `stdio_server.py` ile aynıdır; ajan komutları sonraki fazda gelir.

use std::path::Path;

use evren_api::ApiClient;
use evren_core::config::Config;
use evren_core::{texts, EvrenError};
use evren_protocol::{
    CommandEnvelope, CommandType, ErrorCode, Event, EventType, IdCounter, PROTOCOL_VERSION,
};
use serde_json::Value;

struct Server {
    ids: IdCounter,
    model: String,
    api: Option<ApiClient>,
}

impl Server {
    fn error(&mut self, cid: Option<&str>, code: ErrorCode, message: impl Into<String>) -> Event {
        Event::error(self.ids.event_id(), cid, code, message)
    }

    async fn handle_line(&mut self, line: &str) -> Option<Event> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(self.error(
                    None,
                    ErrorCode::InvalidJson,
                    texts::stdio_invalid_json(&e),
                ))
            }
        };
        let is_valid = value.is_object() && value.get("type").is_some_and(Value::is_string);
        if !is_valid {
            return Some(self.error(
                None,
                ErrorCode::InvalidMessage,
                texts::STDIO_INVALID_MESSAGE,
            ));
        }
        let cid: Option<String> = value.get("id").and_then(Value::as_str).map(str::to_owned);
        let version_ok = match value.get("v") {
            None => true,
            Some(v) => v.as_u64() == Some(u64::from(PROTOCOL_VERSION)),
        };
        if !version_ok {
            return Some(self.error(
                cid.as_deref(),
                ErrorCode::UnsupportedVersion,
                texts::STDIO_UNSUPPORTED_VERSION,
            ));
        }
        let env: CommandEnvelope = match serde_json::from_value(value) {
            Ok(e) => e,
            Err(e) => {
                return Some(self.error(cid.as_deref(), ErrorCode::InvalidMessage, e.to_string()))
            }
        };
        let Some(kind) = CommandType::parse(&env.kind) else {
            return Some(self.error(
                cid.as_deref(),
                ErrorCode::UnknownCommand,
                texts::stdio_unknown_command(&env.kind),
            ));
        };
        Some(self.dispatch(kind, cid.as_deref()).await)
    }

    async fn dispatch(&mut self, kind: CommandType, cid: Option<&str>) -> Event {
        match kind {
            CommandType::ListModels => {
                let list: Vec<Value> = match &self.api {
                    Some(api) => api
                        .list_models()
                        .await
                        .into_iter()
                        .map(Value::from)
                        .collect(),
                    None => Vec::new(),
                };
                Event::new(EventType::Models, self.ids.event_id())
                    .reply_to(cid)
                    .field("models", Value::Array(list))
                    .field("current", self.model.as_str())
            }
            CommandType::Quota => match &self.api {
                Some(api) => {
                    let q = api.get_quota(None).await;
                    Event::new(EventType::Quota, self.ids.event_id())
                        .reply_to(cid)
                        .field("quota", Value::Object(q))
                }
                None => self.error(cid, ErrorCode::Quota, texts::API_KEY_MISSING),
            },
            _ => self.error(cid, ErrorCode::Internal, texts::NOT_PORTED_YET),
        }
    }
}

pub async fn serve(config: &Config, model: &str, workspace: &Path) -> Result<(), EvrenError> {
    let mut out = tokio::io::stdout();
    let mut server = Server {
        ids: IdCounter::new(),
        model: model.to_owned(),
        api: ApiClient::new(config).ok(),
    };
    let ready = Event::ready(
        server.ids.event_id(),
        evren_core::VERSION,
        Some(model),
        &workspace.to_string_lossy(),
    );
    crate::write_line(&mut out, &ready.to_line())
        .await
        .map_err(|e| EvrenError::io(Path::new("<stdout>"), e))?;

    let mut lines = crate::stdin_lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(ev) = server.handle_line(&line).await {
            crate::write_line(&mut out, &ev.to_line())
                .await
                .map_err(|e| EvrenError::io(Path::new("<stdout>"), e))?;
        }
    }
    Ok(())
}
