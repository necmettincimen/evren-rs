//! Plan kipinin uzak/ağ adımları (Python `plan_ops_remote.py`): ssh, scp,
//! http, wait. LLM'siz, deterministik; sırlar plan dosyasına yazılmaz
//! (`${ENV}` ile ortamdan).
//!
//! ```text
//! {"op": "ssh",  "host": "user@ip", "jump": "user@jumpip", "identity": "~/.ssh/id_x", "sudo_pass_env": "HOST_SUDO",
//!                "cmd": "kubectl ...", "stdin_file": "x.yaml",
//!                "expect": "substr", "expect_regex": "...", "save": "out.txt", "save_as": "degisken", "timeout": 120}
//! {"op": "scp",  "src": "yerel/ya/da/user@ip:yol", "dst": "user@ip:yol/ya/da/yerel", "jump": "user@jumpip"}
//! {"op": "http", "method": "POST", "url": "https://...", "headers": {"Authorization": "Bearer ${DRONE_TOKEN}"},
//!                "basic_env": ["ADMIN_USER", "ADMIN_PASS"], "json": {...} | "body_file": "x.json",
//!                "expect_status": [200, 201], "expect": "substr", "save_as": "degisken"}
//! {"op": "wait", "ssh": {...ssh adımı...} | "http": {...http adımı...}, "until": "regex", "interval": 10, "timeout": 900}
//! ```
//!
//! Sonraki adımlarda `{{degisken}}` (save_as) ve `${ENV}` kullanılabilir.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use super::{
    first_chars, flag, int_or, req_str, resolve_in_root, run_capture, str_of, tail, Ctx, Step,
};

const SSH_BASE: [&str; 7] = [
    "ssh",
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=15",
    "-o",
    "StrictHostKeyChecking=accept-new",
];

/// Uzak adım adları (`REMOTE_OPS`).
pub const REMOTE_OPS: [&str; 4] = ["ssh", "scp", "http", "wait"];

/// Uzak adımı çalıştırır; bilinmeyen ad için `None`.
pub fn run_remote(
    op: &str,
    step: &Step,
    root: &Path,
    dry: bool,
    ctx: &mut Ctx,
) -> Option<Result<String, String>> {
    Some(match op {
        "ssh" => op_ssh(step, root, dry, ctx),
        "scp" => op_scp(step, root, dry),
        "http" => op_http(step, root, dry, ctx),
        "wait" => op_wait(step, root, dry, ctx),
        _ => return None,
    })
}

/// Python `re.search` karşılığı; desen geçersizse hata metni.
fn py_search(pattern: &str, text: &str) -> Result<bool, String> {
    let re = fancy_regex::Regex::new(pattern).map_err(|e| e.to_string())?;
    re.is_match(text).map_err(|e| e.to_string())
}

fn check(out: &str, step: &Step) -> Result<(), String> {
    if let Some(exp) = str_of(step, "expect").filter(|s| !s.is_empty()) {
        if !out.contains(&exp) {
            return Err(format!(
                "beklenen metin yok: {}\n{}",
                py_repr(&exp),
                tail(out, 12)
            ));
        }
    }
    if let Some(pat) = str_of(step, "expect_regex").filter(|s| !s.is_empty()) {
        if !py_search(&pat, out)? {
            return Err(format!(
                "beklenen desen yok: {}\n{}",
                py_repr(&pat),
                tail(out, 12)
            ));
        }
    }
    Ok(())
}

/// `repr(str)`: tek tırnaklı; metinde tek tırnak varsa çift tırnak.
pub(super) fn py_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// `os.path.expanduser`.
fn expand_user(p: &str) -> String {
    if let Some(rest) = p.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with(['/', '\\']) {
            if let Some(home) = dirs::home_dir() {
                return format!("{}{rest}", home.display());
            }
        }
    }
    p.to_owned()
}

/// `shlex.quote`.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_owned();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
    if safe {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

fn missing_cmd_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)(: command not found|: komut bulunamadı)$")
            .unwrap_or_else(|_| Regex::new(r"a^").unwrap_or_else(|_| unreachable!("sabit desen")))
    })
}

pub(super) fn ok_exit(step: &Step, code: i32) -> bool {
    match step.get("ok_exit").and_then(Value::as_array) {
        Some(list) => list.iter().any(|v| v.as_i64() == Some(i64::from(code))),
        None => code == 0,
    }
}

fn ssh_run(step: &Step, root: &Path) -> Result<String, String> {
    let mut args: Vec<String> = SSH_BASE[1..].iter().map(|s| (*s).to_owned()).collect();
    if let Some(j) = str_of(step, "jump").filter(|s| !s.is_empty()) {
        args.extend(["-J".to_owned(), j]);
    }
    if let Some(i) = str_of(step, "identity").filter(|s| !s.is_empty()) {
        args.extend(["-i".to_owned(), expand_user(&i)]);
    }
    let mut remote = req_str(step, "cmd")?;
    let mut stdin: Option<Vec<u8>> = match str_of(step, "stdin_file").filter(|s| !s.is_empty()) {
        Some(f) => {
            let p = resolve_in_root(root, &f)?;
            Some(std::fs::read(&p).map_err(|e| format!("{e}: '{}'", p.display()))?)
        }
        None => None,
    };
    if let Some(var) = str_of(step, "sudo_pass_env").filter(|s| !s.is_empty()) {
        // Parola plan dosyasına yazılmaz; sudo -S stdin'den okur, komut root kabuğunda çalışır.
        let pw = std::env::var(&var).map_err(|_| format!("ortam değişkeni yok: {var}"))?;
        // sudo secure_path'te /usr/local/bin yok (k3s kubectl orada).
        remote = format!(
            "sudo -S -p '' bash -c {}",
            shlex_quote(&format!(
                "export PATH=/usr/local/sbin:/usr/local/bin:$PATH; {remote}"
            ))
        );
        let mut data = pw.into_bytes();
        data.push(b'\n');
        data.extend(stdin.unwrap_or_default());
        stdin = Some(data);
    }
    args.push(req_str(step, "host")?);
    args.push(remote);
    let mut cmd = Command::new(SSH_BASE[0]);
    cmd.args(&args);
    let secs = int_or(step, "timeout", 180).max(1) as u64;
    let r = run_capture(&mut cmd, stdin, Duration::from_secs(secs))?;
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );
    if !ok_exit(step, r.code) {
        return Err(format!("ssh çıkış kodu {}\n{}", r.code, tail(&out, 12)));
    }
    // `a; b | head` biçiminde b bulunamasa da çıkış kodu 0 döner; sessiz başarı sayılmasın.
    if !flag(step, "allow_missing_cmd", false) && missing_cmd_re().is_match(&out) {
        return Err(format!(
            "uzak komut bulunamadı (allow_missing_cmd ile susturulabilir)\n{}",
            tail(&out, 12)
        ));
    }
    Ok(out)
}

fn op_ssh(step: &Step, root: &Path, dry: bool, ctx: &mut Ctx) -> Result<String, String> {
    if dry {
        return Ok(format!(
            "ssh {}: {}",
            req_str(step, "host")?,
            first_chars(&req_str(step, "cmd")?, 80)
        ));
    }
    let out = ssh_run(step, root)?;
    check(&out, step)?;
    save(step, &out, root, ctx)?;
    let last = first_chars(&tail(&out, 1), 160).to_owned();
    Ok(if last.is_empty() {
        "tamam".into()
    } else {
        last
    })
}

fn op_scp(step: &Step, root: &Path, dry: bool) -> Result<String, String> {
    let src = req_str(step, "src")?;
    let dst = req_str(step, "dst")?;
    if dry {
        return Ok(format!("scp {src} -> {dst}"));
    }
    let mut cmd = Command::new("scp");
    cmd.args(["-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]);
    if let Some(j) = str_of(step, "jump").filter(|s| !s.is_empty()) {
        cmd.args(["-J", &j]);
    }
    if let Some(i) = str_of(step, "identity").filter(|s| !s.is_empty()) {
        cmd.args(["-i", &expand_user(&i)]);
    }
    let local = |v: &str| -> Result<String, String> {
        let remote = v.contains(':') && v.split(':').next().is_some_and(|h| h.contains('@'));
        if remote {
            Ok(v.to_owned())
        } else {
            Ok(resolve_in_root(root, v)?.display().to_string())
        }
    };
    cmd.args([local(&src)?, local(&dst)?]);
    let secs = int_or(step, "timeout", 300).max(1) as u64;
    let r = run_capture(&mut cmd, None, Duration::from_secs(secs))?;
    if r.code != 0 {
        return Err(tail(
            &format!(
                "{}{}",
                String::from_utf8_lossy(&r.stdout),
                String::from_utf8_lossy(&r.stderr)
            ),
            12,
        ));
    }
    Ok("kopyalandı".into())
}

/// Base64 (standart alfabe, dolgulu).
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `json.dumps(value)` (Python varsayılanı: `ensure_ascii=True`, `", "`/`": "` ayraçları).
pub fn py_json_ascii(value: &Value) -> String {
    let text = evren_agent::usage_log::py_dumps(value);
    let mut out = String::with_capacity(text.len());
    let mut buf = [0u16; 2];
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

fn http_run(step: &Step, root: &Path) -> Result<(u16, String), String> {
    let mut headers: Vec<(String, String)> = step
        .get("headers")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let v = v
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string());
                    (k.clone(), v)
                })
                .collect()
        })
        .unwrap_or_default();
    let has =
        |h: &[(String, String)], name: &str| h.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if let Some(keys) = step.get("basic_env").and_then(Value::as_array) {
        if !keys.is_empty() {
            let mut vals = Vec::new();
            for k in keys.iter().take(2) {
                let k = k.as_str().unwrap_or_default();
                vals.push(std::env::var(k).map_err(|_| format!("'{k}'"))?);
            }
            let (user, pw) = (
                vals.first().cloned().unwrap_or_default(),
                vals.get(1).cloned().unwrap_or_default(),
            );
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case("Authorization"));
            headers.push((
                "Authorization".into(),
                format!("Basic {}", base64_encode(format!("{user}:{pw}").as_bytes())),
            ));
        }
    }
    if !has(&headers, "User-Agent") {
        headers.push(("User-Agent".into(), "curl/8.4.0".into()));
    }
    let mut data: Option<Vec<u8>> = None;
    match step.get("json") {
        Some(v) if !v.is_null() => {
            data = Some(py_json_ascii(v).into_bytes());
            if !has(&headers, "Content-Type") {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
        }
        _ => {
            if let Some(f) = str_of(step, "body_file").filter(|s| !s.is_empty()) {
                let p = resolve_in_root(root, &f)?;
                data = Some(std::fs::read(&p).map_err(|e| format!("{e}: '{}'", p.display()))?);
                if !has(&headers, "Content-Type") {
                    headers.push(("Content-Type".into(), "application/json".into()));
                }
            }
        }
    }
    let method = str_of(step, "method").unwrap_or_else(|| "GET".into());
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let secs = int_or(step, "timeout", 60).max(1) as u64;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(secs))
        .danger_accept_invalid_certs(flag(step, "insecure", false))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.request(method, req_str(step, "url")?);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(body) = data {
        req = req.body(body);
    }
    let resp = req.send().map_err(|e| format!("<urlopen error {e}>"))?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().map_err(|e| e.to_string())?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

fn op_http(step: &Step, root: &Path, dry: bool, ctx: &mut Ctx) -> Result<String, String> {
    if dry {
        return Ok(format!(
            "{} {}",
            str_of(step, "method").unwrap_or_else(|| "GET".into()),
            first_chars(&req_str(step, "url")?, 80)
        ));
    }
    let (status, body) = http_run(step, root)?;
    let ok = match step.get("expect_status").and_then(Value::as_array) {
        Some(list) => list.iter().any(|v| v.as_u64() == Some(u64::from(status))),
        None => [200, 201, 202, 204].contains(&status),
    };
    if !ok {
        return Err(format!("HTTP {status}\n{}", tail(&body, 12)));
    }
    check(&body, step)?;
    save(step, &body, root, ctx)?;
    Ok(format!("HTTP {status}"))
}

fn op_wait(step: &Step, root: &Path, dry: bool, ctx: &mut Ctx) -> Result<String, String> {
    if dry {
        return Ok("bekleme (kuru çalıştırmada atlandı)".into());
    }
    let deadline = Instant::now() + Duration::from_secs(int_or(step, "timeout", 900).max(0) as u64);
    let until = fancy_regex::Regex::new(&req_str(step, "until")?).map_err(|e| e.to_string())?;
    let interval = Duration::from_secs(int_or(step, "interval", 10).max(0) as u64);
    let mut last = String::new();
    while Instant::now() < deadline {
        let sub = |k: &str| step.get(k).and_then(Value::as_object).cloned();
        let res = if step.contains_key("ssh") {
            match sub("ssh") {
                Some(s) => ssh_run(&s, root),
                None => Err("'ssh'".into()),
            }
        } else {
            match sub("http") {
                Some(h) => http_run(&h, root).map(|(st, body)| format!("{st} {body}")),
                None => Err("'http'".into()),
            }
        };
        last = match res {
            Ok(text) => text,
            Err(e) => format!("hata: {e}"),
        };
        if until.is_match(&last).unwrap_or(false) {
            save(step, &last, root, ctx)?;
            return Ok("koşul sağlandı".into());
        }
        std::thread::sleep(interval);
    }
    Err(format!("zaman aşımı; son çıktı:\n{}", tail(&last, 12)))
}

fn save(step: &Step, text: &str, root: &Path, ctx: &mut Ctx) -> Result<(), String> {
    if let Some(rel) = str_of(step, "save").filter(|s| !s.is_empty()) {
        let path = resolve_in_root(root, &rel)?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
    }
    if let Some(name) = str_of(step, "save_as").filter(|s| !s.is_empty()) {
        ctx.insert(name, text.trim().to_owned());
    }
    Ok(())
}

fn ctx_re() -> &'static [Regex; 2] {
    static RE: OnceLock<[Regex; 2]> = OnceLock::new();
    RE.get_or_init(|| {
        let mk = |p: &str| Regex::new(p).unwrap_or_else(|_| unreachable!("sabit desen"));
        [mk(r"\{\{(\w+)\}\}"), mk(r"\$\{(\w+)\}")]
    })
}

/// `{{degisken}}` (önceki adımların `save_as` değerleri) ve `${ENV}` yerine
/// koyar; yinelemeli (nesne/dizi). Anahtarlar değişmez.
pub fn expand(value: &Value, ctx: &HashMap<String, String>) -> Value {
    match value {
        Value::String(s) => {
            let [vars, env] = ctx_re();
            let s = vars.replace_all(s, |c: &regex::Captures<'_>| {
                ctx.get(&c[1]).cloned().unwrap_or_else(|| c[0].to_owned())
            });
            let s = env.replace_all(&s, |c: &regex::Captures<'_>| {
                std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_owned())
            });
            Value::String(s.into_owned())
        }
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, v)| (k.clone(), expand(v, ctx))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| expand(v, ctx)).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expand_replaces_ctx_and_env_recursively() {
        std::env::set_var("EVREN_PLAN_TEST_X", "ortam");
        let mut ctx = HashMap::new();
        ctx.insert("ad".to_owned(), "değer".to_owned());
        let v = json!({"a": "{{ad}}-${EVREN_PLAN_TEST_X}", "b": ["{{yok}}", "${EVREN_PLAN_YOK_Q}"], "n": 3});
        let out = expand(&v, &ctx);
        assert_eq!(out["a"], "değer-ortam");
        assert_eq!(out["b"][0], "{{yok}}");
        assert_eq!(out["b"][1], "${EVREN_PLAN_YOK_Q}");
        assert_eq!(out["n"], 3);
    }

    #[test]
    fn shlex_and_base64_match_python() {
        assert_eq!(shlex_quote("abc-1.2"), "abc-1.2");
        assert_eq!(shlex_quote("a b'c"), "'a b'\"'\"'c'");
        assert_eq!(shlex_quote(""), "''");
        assert_eq!(base64_encode(b"user:pa"), "dXNlcjpwYQ==");
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(
            py_json_ascii(&json!({"a": "ç", "b": [1, 2]})),
            r#"{"a": "\u00e7", "b": [1, 2]}"#
        );
    }

    #[test]
    fn dry_runs_describe_the_step() {
        let root = std::env::temp_dir();
        let mut ctx = HashMap::new();
        let step = |v: Value| v.as_object().cloned().unwrap_or_default();
        let s = step(json!({"host": "u@h", "cmd": "uptime"}));
        assert_eq!(
            op_ssh(&s, &root, true, &mut ctx),
            Ok("ssh u@h: uptime".into())
        );
        let s = step(json!({"url": "https://x/y"}));
        assert_eq!(
            op_http(&s, &root, true, &mut ctx),
            Ok("GET https://x/y".into())
        );
        let s = step(json!({"src": "a", "dst": "u@h:/b"}));
        assert_eq!(op_scp(&s, &root, true), Ok("scp a -> u@h:/b".into()));
        let s = step(json!({"until": "x"}));
        assert_eq!(
            op_wait(&s, &root, true, &mut ctx),
            Ok("bekleme (kuru çalıştırmada atlandı)".into())
        );
        let s = step(json!({"cmd": "x"}));
        assert_eq!(op_ssh(&s, &root, true, &mut ctx), Err("'host'".into()));
    }
}
