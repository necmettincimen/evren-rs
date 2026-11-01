//! Sahte LLM sunucusuna (`evren-cli/scripts/fake_llm_server.py`) karşı
//! model listesi, akışsız ve akışlı sohbet testleri. Python ya da betik
//! yoksa test atlanır.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use evren_api::{ApiClient, ChatRequest, StreamDelta};
use evren_core::config::Config;
use serde_json::json;

struct FakeServer {
    child: Child,
    base_url: String,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn script_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("EVREN_CLI_DIR") {
        let p = PathBuf::from(dir)
            .join("scripts")
            .join("fake_llm_server.py");
        if p.is_file() {
            return Some(p);
        }
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("evren-cli")
        .join("scripts")
        .join("fake_llm_server.py");
    p.is_file().then_some(p)
}

fn spawn_python(script: &PathBuf, port_file: &PathBuf) -> Option<Child> {
    let candidates: [(&str, &[&str]); 3] = [("python", &[]), ("py", &["-3"]), ("python3", &[])];
    for (exe, pre) in candidates {
        let mut cmd = Command::new(exe);
        cmd.args(pre)
            .arg("-I")
            .arg(script)
            .arg("--port-file")
            .arg(port_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(child) = cmd.spawn() {
            return Some(child);
        }
    }
    None
}

fn start_server(dir: &tempfile::TempDir) -> Option<FakeServer> {
    let Some(script) = script_path() else {
        eprintln!("atlandı: fake_llm_server.py bulunamadı");
        return None;
    };
    let port_file = dir.path().join("port.txt");
    let Some(mut child) = spawn_python(&script, &port_file) else {
        eprintln!("atlandı: python bulunamadı");
        return None;
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let port = loop {
        if let Ok(s) = std::fs::read_to_string(&port_file) {
            if let Ok(p) = s.trim().parse::<u16>() {
                break Some(p);
            }
        }
        if let Ok(Some(_)) = child.try_wait() {
            break None;
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let Some(port) = port else {
        let _ = child.kill();
        eprintln!("atlandı: sahte sunucu port dosyasını yazmadı");
        return None;
    };
    Some(FakeServer {
        child,
        base_url: format!("http://127.0.0.1:{port}/v1"),
    })
}

fn client_for(server: &FakeServer) -> ApiClient {
    let env: BTreeMap<String, String> = [
        ("EVREN_API_KEY", "evren_llm_test"),
        ("EVREN_BASE_URL", server.base_url.as_str()),
        ("EVREN_DEFAULT_MODEL", "fake-model"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    ApiClient::new(&Config::from_env(env)).unwrap()
}

#[tokio::test]
async fn models_chat_and_stream_against_fake_server() {
    let dir = tempfile::tempdir().unwrap();
    let Some(server) = start_server(&dir) else {
        return;
    };
    let api = client_for(&server);

    let models = api.list_models().await;
    assert!(models.contains(&"fake-model".to_owned()), "{models:?}");
    assert!(models.contains(&"fake-model-2".to_owned()), "{models:?}");

    let resp = api
        .chat_complete(ChatRequest::simple("sistem", "merhaba"))
        .await
        .unwrap();
    assert_eq!(
        resp.content().map(str::trim),
        Some("Merhaba, ben sahte model.")
    );
    assert_eq!(resp.finish_reason(), Some("stop"));

    let mut text = String::new();
    let mut deltas = 0usize;
    let resp = api
        .chat_complete_stream(ChatRequest::simple("sistem", "merhaba"), |d| {
            if let StreamDelta::Content(c) = d {
                text.push_str(&c);
                deltas += 1;
            }
        })
        .await
        .unwrap();
    assert_eq!(text, "Merhaba, ben sahte model. ");
    assert!(deltas >= 4, "parça sayısı: {deltas}");
    assert_eq!(resp.content(), Some("Merhaba, ben sahte model. "));
    assert_eq!(resp.finish_reason(), Some("stop"));
    assert_eq!(
        resp.usage().and_then(|u| u.get("total_tokens")).cloned(),
        Some(json!(15))
    );

    let mut last_calls = Vec::new();
    let resp = api
        .chat_complete_stream(ChatRequest::simple("sistem", "dosya: notlar.txt"), |d| {
            if let StreamDelta::ToolCalls(t) = d {
                last_calls = t;
            }
        })
        .await
        .unwrap();
    assert_eq!(resp.finish_reason(), Some("tool_calls"));
    let calls = resp.tool_calls().cloned().unwrap_or_default();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_fake_1");
    assert_eq!(calls[0]["function"]["name"], "write_file");
    let args: serde_json::Value =
        serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args["path"], "notlar.txt");
    assert_eq!(last_calls.len(), 1);
}
