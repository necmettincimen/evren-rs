//! En küçük stdio MCP istemcisi (Python `mcp_client.py`): JSON-RPC 2.0, satır ayrımlı;
//! `initialize`, `tools/list`, `tools/call`. Sunucular `mcpServers` ayarından okunur ve
//! ilk listede tembel başlatılır; araçlar `mcp__<sunucu>__<araç>` adıyla sunulur.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::{ApprovalKind, ApprovalRequest, ToolContext};
use crate::settings::Settings;

pub const PROTOCOL: &str = "2024-11-05";
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

struct McpServer {
    name: String,
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: String,
    proc: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: Option<Receiver<Value>>,
    next_id: i64,
    tools: Vec<Value>,
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl McpServer {
    fn from_config(name: &str, cfg: &Map<String, Value>, cwd: &Path) -> Option<Self> {
        let command = cfg
            .get("command")
            .map(value_text)
            .filter(|c| !c.is_empty())?;
        let args = cfg
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(value_text).collect())
            .unwrap_or_default();
        let env = cfg
            .get("env")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), value_text(v))).collect())
            .unwrap_or_default();
        Some(McpServer {
            name: name.to_string(),
            command,
            args,
            env,
            cwd: cwd.to_string_lossy().to_string(),
            proc: None,
            stdin: None,
            rx: None,
            next_id: 0,
            tools: Vec::new(),
        })
    }

    fn running(&mut self) -> bool {
        self.proc
            .as_mut()
            .is_some_and(|p| matches!(p.try_wait(), Ok(None)))
    }

    fn start(&mut self) -> Result<(), String> {
        if self.running() {
            return Ok(());
        }
        // Python `shell=(os.name == "nt")`: Windows'ta `npx` gibi .cmd sarmalayıcıları bulunur.
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&self.command);
            c
        } else {
            Command::new(&self.command)
        };
        cmd.args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("MCP '{}' başlatılamadı: {e}", self.name))?;
        let stdout = child.stdout.take().ok_or("stdout yok")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(msg) = serde_json::from_str::<Value>(&line) {
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
            }
        });
        self.stdin = child.stdin.take();
        self.proc = Some(child);
        self.rx = Some(rx);
        self.request(
            "initialize",
            json!({"protocolVersion": PROTOCOL, "capabilities": {},
                   "clientInfo": {"name": "evren", "version": "1"}}),
            LIST_TIMEOUT,
        )?;
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        let listed = self.request("tools/list", json!({}), LIST_TIMEOUT)?;
        self.tools = listed
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(())
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("MCP sunucusu çalışmıyor")?;
        writeln!(stdin, "{msg}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("MCP '{}' yazılamadı: {e}", self.name))
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        self.next_id += 1;
        let rid = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}))?;
        let timed_out = || format!("MCP '{}' yanıt vermedi ({method})", self.name);
        let rx = self.rx.as_ref().ok_or("MCP sunucusu çalışmıyor")?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = match rx.recv_timeout(left) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => return Err(timed_out()),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!("MCP '{}' bağlantısı kapandı ({method})", self.name))
                }
            };
            if msg.get("id").and_then(Value::as_i64) != Some(rid) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                return Err(err
                    .get("message")
                    .map(value_text)
                    .unwrap_or_else(|| err.to_string()));
            }
            return Ok(msg
                .get("result")
                .filter(|r| !r.is_null())
                .cloned()
                .unwrap_or_else(|| json!({})));
        }
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Result<String, String> {
        self.start()?;
        let res = self.request(
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
            CALL_TIMEOUT,
        )?;
        let content = res.get("content").cloned().unwrap_or_else(|| json!([]));
        let text = content
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                    .map(|c| c.get("text").and_then(Value::as_str).unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let body = if text.is_empty() {
            content.to_string().chars().take(4000).collect()
        } else {
            text
        };
        let is_error = res.get("isError").and_then(Value::as_bool) == Some(true);
        Ok(format!("{}{body}", if is_error { "HATA: " } else { "" }))
    }

    fn stop(&mut self) {
        self.stdin = None;
        if let Some(mut p) = self.proc.take() {
            if matches!(p.try_wait(), Ok(None)) {
                let _ = p.kill();
                let _ = p.wait();
            }
        }
    }
}

static SERVERS: LazyLock<Mutex<HashMap<String, McpServer>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `mcpServers` ayarı (birleşik ayarlar; yoksa boş).
pub fn configured(root: &Path) -> Map<String, Value> {
    Settings::load(Some(root))
        .data
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn with_server<R>(
    cfg: &Map<String, Value>,
    name: &str,
    root: &Path,
    f: impl FnOnce(&mut McpServer) -> R,
) -> Option<R> {
    let mut servers = SERVERS.lock().unwrap_or_else(|e| e.into_inner());
    if !servers.contains_key(name) {
        let srv = McpServer::from_config(name, cfg.get(name)?.as_object()?, root)?;
        servers.insert(name.to_string(), srv);
    }
    servers.get_mut(name).map(f)
}

/// Tanımlı tüm sunucuların araç şemaları; başlatılamayan sunucu atlanır.
pub fn schemas(root: &Path) -> Vec<Value> {
    schemas_from(&configured(root), root)
}

fn schemas_from(cfg: &Map<String, Value>, root: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    for name in cfg.keys() {
        let tools = with_server(cfg, name, root, |srv| {
            srv.start().ok().map(|()| srv.tools.clone())
        });
        for t in tools.flatten().unwrap_or_default() {
            let tool = t.get("name").map(value_text).unwrap_or_default();
            let desc: String = t
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            let params = t
                .get("inputSchema")
                .filter(|s| s.is_object())
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            out.push(json!({"type": "function", "function": {
                "name": format!("mcp__{name}__{tool}"),
                "description": format!("{desc} (MCP: {name})"),
                "parameters": params,
            }}));
        }
    }
    out
}

/// `mcp__<sunucu>__<araç>` çağrısı; kullanıcı onayı ister (otomatik onay yoksa).
pub fn call_tool(
    ctx: &ToolContext,
    full_name: &str,
    arguments: Value,
    auto_approve: bool,
) -> String {
    call_tool_from(
        &configured(&ctx.root()),
        ctx,
        full_name,
        arguments,
        auto_approve,
    )
}

fn call_tool_from(
    cfg: &Map<String, Value>,
    ctx: &ToolContext,
    full_name: &str,
    arguments: Value,
    auto_approve: bool,
) -> String {
    let mut parts = full_name.splitn(3, "__").skip(1);
    let (server, tool) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let root = ctx.root();
    if with_server(cfg, server, &root, |_| ()).is_none() {
        return format!("HATA: MCP sunucusu tanımlı değil: {server}");
    }
    if let Some(denied) = ctx.request_approval(auto_approve, mcp_request(full_name)) {
        return denied;
    }
    match with_server(cfg, server, &root, |srv| srv.call(tool, arguments)) {
        Some(Ok(text)) => text,
        Some(Err(e)) => format!("HATA: MCP çağrısı başarısız: {e}"),
        None => format!("HATA: MCP sunucusu tanımlı değil: {server}"),
    }
}

/// `prompt_confirm("MCP aracı '...' çalıştırılsın mı?", default=False)`.
pub fn mcp_request(full_name: &str) -> ApprovalRequest {
    ApprovalRequest {
        kind: ApprovalKind::Mcp,
        ui_kind: "other",
        title: format!("MCP aracı '{full_name}' çalıştırılsın mı?"),
        detail: full_name.to_string(),
        default_yes: false,
        proposal: None,
        warning: None,
    }
}

/// Başlatılan tüm MCP sunucularını kapatır (REPL çıkışı).
pub fn shutdown() {
    let mut servers = SERVERS.lock().unwrap_or_else(|e| e.into_inner());
    for srv in servers.values_mut() {
        srv.stop();
    }
    servers.clear();
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Test ikilisinin kendisi sahte MCP sunucusu olarak çalışır (`EVREN_FAKE_MCP=1`).
    #[test]
    fn fake_server() {
        if std::env::var("EVREN_FAKE_MCP").is_err() {
            return;
        }
        let mut out = std::io::stdout();
        // Test düzeneğinin "test ... " öneki ilk satırı bozmasın.
        writeln!(out).unwrap();
        for line in std::io::stdin().lock().lines() {
            let Ok(msg) = serde_json::from_str::<Value>(&line.unwrap()) else {
                continue;
            };
            let Some(id) = msg.get("id").cloned() else {
                continue;
            };
            let result = match msg["method"].as_str().unwrap_or("") {
                "initialize" => json!({"protocolVersion": PROTOCOL}),
                "tools/list" => json!({"tools": [{"name": "echo", "description": "Yankı",
                    "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}),
                "tools/call" => {
                    let text = msg["params"]["arguments"]["text"].as_str().unwrap_or("");
                    json!({"content": [{"type": "text", "text": text}], "isError": text == "bozuk"})
                }
                _ => {
                    writeln!(
                        out,
                        "{}",
                        json!({"jsonrpc": "2.0", "id": id,
                        "error": {"message": "bilinmeyen yöntem"}})
                    )
                    .unwrap();
                    out.flush().unwrap();
                    continue;
                }
            };
            writeln!(
                out,
                "{}",
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            )
            .unwrap();
            out.flush().unwrap();
        }
        std::process::exit(0);
    }

    fn fake_config(name: &str) -> Map<String, Value> {
        let exe = std::env::current_exe().unwrap();
        let cfg = json!({name: {
            "command": exe.to_string_lossy(),
            "args": ["tools::mcp::tests::fake_server", "--exact", "--nocapture", "--test-threads=1"],
            "env": {"EVREN_FAKE_MCP": "1"},
        }});
        cfg.as_object().unwrap().clone()
    }

    #[test]
    fn lists_and_calls_fake_server() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_config("sahte");
        let schemas = schemas_from(&cfg, dir.path());
        assert_eq!(schemas.len(), 1, "{schemas:?}");
        assert_eq!(schemas[0]["function"]["name"], "mcp__sahte__echo");
        assert_eq!(schemas[0]["function"]["description"], "Yankı (MCP: sahte)");
        let ctx = ToolContext::auto(dir.path());
        let ok = call_tool_from(
            &cfg,
            &ctx,
            "mcp__sahte__echo",
            json!({"text": "merhaba"}),
            true,
        );
        assert_eq!(ok, "merhaba");
        let bad = call_tool_from(
            &cfg,
            &ctx,
            "mcp__sahte__echo",
            json!({"text": "bozuk"}),
            true,
        );
        assert_eq!(bad, "HATA: bozuk");
        let missing = call_tool_from(&cfg, &ctx, "mcp__yok__echo", json!({}), true);
        assert_eq!(missing, "HATA: MCP sunucusu tanımlı değil: yok");
        let mut servers = SERVERS.lock().unwrap();
        servers.get_mut("sahte").unwrap().stop();
        servers.remove("sahte");
    }
}
