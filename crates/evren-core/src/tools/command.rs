//! `run_command`, `check_background`, `stop_background` (Python `tools.py`).

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use super::background::{shell_command, MAX_CHECK_CHARS};
use super::proc::{stream_with_tree_kill, StreamTag};
use super::{background_request, command_request, ToolContext};

pub const DEFAULT_COMMAND_TIMEOUT: i64 = 180;
pub const MAX_COMMAND_TIMEOUT: i64 = 600;
/// Zaman aşımıyla öldürülen komutun geleneksel çıkış kodu (coreutils `timeout`).
pub const TIMEOUT_EXIT_CODE: i32 = 124;
pub const OUTPUT_HEAD_CHARS: usize = 6000;
pub const OUTPUT_TAIL_CHARS: usize = 2000;
/// Bu boyutun üstündeki çıktı bağlama gömülmez, dosyaya yazılır (`spill_long_output`).
pub const BIG_OUTPUT_CHARS: usize = 100_000;

pub const CHECK_DEFAULT_WAIT: i64 = 20;
pub const CHECK_MAX_WAIT: i64 = 300;

/// cmd.exe'nin sessizce bozacağı çok satırlı komut.
pub const MULTILINE_SHELL_MESSAGE: &str = "cmd.exe çok satırlı komut çalıştıramaz. Python kodu için use_python=true, PowerShell için use_powershell=true ver ya da satırları && ile birleştir.";

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn last_chars(s: &str, n: usize) -> &str {
    let total = char_len(s);
    if n >= total {
        return s;
    }
    match s.char_indices().nth(total - n) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

/// Baş + kuyruk tutar, ortayı atar (`_truncate_output`).
pub fn truncate_output(text: &str) -> String {
    let n = char_len(text);
    if n <= OUTPUT_HEAD_CHARS + OUTPUT_TAIL_CHARS {
        return text.to_string();
    }
    let head = take_chars(text, OUTPUT_HEAD_CHARS);
    let tail = last_chars(text, OUTPUT_TAIL_CHARS);
    let dropped = n - OUTPUT_HEAD_CHARS - OUTPUT_TAIL_CHARS;
    format!("{head}\n... [{dropped} karakter kırpıldı] ...\n{tail}")
}

/// Gövdesinde bilgi taşımayan dev çıktı için tek satırlık boyut özeti.
fn single_line_summary(text: &str) -> String {
    let n = char_len(text);
    let lines = text.matches('\n').count() + 1;
    let stripped = text.trim();
    if stripped.is_empty() {
        return format!("[EVREN] Çıktı {n} karakter, {lines} satır (boşluk).");
    }
    let first = crate::output_summary::splitlines(stripped)
        .first()
        .copied()
        .unwrap_or("");
    let distinct: std::collections::HashSet<char> = stripped.chars().collect();
    if distinct.len() <= 2 {
        return format!(
            "[EVREN] Çıktı {n} karakter, {lines} satır; gövde tek karakterin tekrarı ('{}'), bağlama gömülmedi.",
            take_chars(first, 1)
        );
    }
    format!(
        "[EVREN] Çıktı {n} karakter, {lines} satır; gövde bağlama gömülmedi (ilk satır: '{}').",
        take_chars(first, 80)
    )
}

/// Uzun çıktı (Python `ssh_ops.spill_long_output`): `.evren/tmp/<label>-<zaman>.txt`
/// dosyasına kaydedilir; modele türe duyarlı özet ya da baş/kuyruk görünümü ile
/// "tabloyu yeniden yazma" notu gider. Kısa çıktı olduğu gibi döner.
pub fn spill_long_output(text: &str, root: Option<&Path>, label: &str, command: &str) -> String {
    let n = char_len(text);
    let lines = text.matches('\n').count() + 1;
    // Log çıktısı (Loki, kubectl logs, heartbeat) kısa olsa da satır çoksa özetlenir.
    let short_log = lines > crate::output_summary::LOG_MIN_LINES
        && crate::output_summary::is_log_output(text, command);
    if n <= OUTPUT_HEAD_CHARS + OUTPUT_TAIL_CHARS && !short_log {
        return text.to_string();
    }
    let saved = root.and_then(|root| {
        let dir = root.join(".evren").join("tmp");
        std::fs::create_dir_all(&dir).ok()?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%f").to_string();
        let path = dir.join(format!("{label}-{stamp}.txt"));
        std::fs::write(&path, text).ok()?;
        Some(
            path.strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/"),
        )
    });
    let place = match &saved {
        Some(rel) => format!(" ve {rel} dosyasına kaydedildi"),
        None => String::new(),
    };
    let mut view = truncate_output(text);
    match crate::output_summary::summarize_output(text, command) {
        Some(summary) if char_len(&summary) < char_len(&view) => {
            view = format!("[EVREN] Çıktı özetlendi ({lines} satır).\n{summary}");
        }
        _ if n > BIG_OUTPUT_CHARS => {
            return format!(
                "{}\n[EVREN] Tam çıktı kullanıcının terminalinde gösterildi{place}. Gövdeyi yanıtında \
                 yeniden yazma; soru yalnızca boyut/karakter sayısıysa yukarıdaki sayıyı kullan ve hemen bitir.{}",
                single_line_summary(text),
                if saved.is_some() { " Gerekirse view_file ile dosyadan aralık oku." } else { "" }
            );
        }
        _ => {}
    }
    format!(
        "{view}\n[EVREN] Tam çıktı ({lines} satır) kullanıcının terminalinde gösterildi{place}. \
         Listeyi/tabloyu yanıtında yeniden yazma. Görev listeleme/gösterme ise iş bitti: ek analiz \
         komutu çalıştırmadan, gördüğün kısma dayanarak 1-3 cümlelik kısa özetle hemen bitir. \
         Yalnızca kullanıcı açıkça analiz/süzme istediyse komutu süzerek (grep/awk/jsonpath) yeniden çalıştır{}",
        if saved.is_some() { " veya view_file ile dosyadan aralık oku." } else { "." }
    )
}

/// Çalıştırılacak komut: kabuk metni ya da argüman listesi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Argv {
    Shell(String),
    List(Vec<String>),
}

impl Argv {
    pub fn to_command(&self) -> Command {
        match self {
            Argv::Shell(text) => shell_command(text),
            Argv::List(list) => {
                let mut c = Command::new(list.first().map(String::as_str).unwrap_or(""));
                c.args(list.iter().skip(1));
                c
            }
        }
    }

    pub fn is_shell(&self) -> bool {
        matches!(self, Argv::Shell(_))
    }
}

fn inline_python_re() -> &'static [Regex; 2] {
    static RE: OnceLock<[Regex; 2]> = OnceLock::new();
    RE.get_or_init(|| {
        let dq = Regex::new(r#"(?s)^\s*(?:python3?|py)(?:\.exe)?\s+-c\s+"(?P<code>.*)"\s*$"#);
        let sq = Regex::new(r"(?s)^\s*(?:python3?|py)(?:\.exe)?\s+-c\s+'(?P<code>.*)'\s*$");
        match (dq, sq) {
            (Ok(a), Ok(b)) => [a, b],
            _ => [never_re(), never_re()],
        }
    })
}

fn powershell_wrapper_re() -> &'static [Regex; 2] {
    static RE: OnceLock<[Regex; 2]> = OnceLock::new();
    RE.get_or_init(|| {
        let head = r"(?is)^\s*(?:powershell|pwsh)(?:\.exe)?(?:\s+-(?:NoProfile|NonInteractive|NoLogo|ExecutionPolicy\s+\S+))*\s+-Command\s+";
        let dq = Regex::new(&format!(r#"{head}"(?P<body>.*)"\s*$"#));
        let sq = Regex::new(&format!(r"{head}'(?P<body>.*)'\s*$"));
        match (dq, sq) {
            (Ok(a), Ok(b)) => [a, b],
            _ => [never_re(), never_re()],
        }
    })
}

pub(crate) fn never_re() -> Regex {
    Regex::new(r"\A(?-u:\x00)\z")
        .unwrap_or_else(|_| Regex::new("x").unwrap_or_else(|_| never_fallback()))
}

fn never_fallback() -> Regex {
    // `"x"` her zaman derlenir; buraya ulaşılmaz.
    #[allow(clippy::unwrap_used)]
    Regex::new("x").unwrap()
}

fn win_filter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(\|\s*)(find|sort)(\s+(?:/[a-zA-Z]|"))"#).unwrap_or_else(|_| never_re())
    })
}

/// `| find /c ...` ve `| sort /r` Windows araçlarına sabitlenir (`_pin_windows_filters`).
pub fn pin_windows_filters(command: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    win_filter_re()
        .replace_all(command, |caps: &regex::Captures| {
            format!(
                "{}{}\\System32\\{}.exe{}",
                &caps[1],
                root,
                caps[2].to_lowercase(),
                &caps[3]
            )
        })
        .into_owned()
}

fn grep_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\bgrep\b((?:\s+-[A-Za-z0-9=,.-]+)*)").unwrap_or_else(|_| never_re())
    })
}

const GREP_EXCLUDES: &str =
    " --exclude-dir=bin --exclude-dir=obj --exclude-dir=node_modules --exclude-dir=.git --exclude-dir=.evren";

/// Özyinelemeli `grep`'e `--exclude-dir` ekler (`_speed_grep`).
pub fn speed_grep(command: &str) -> String {
    if command.contains("--exclude-dir") || command.contains("git grep") {
        return command.to_string();
    }
    grep_re()
        .replace_all(command, |caps: &regex::Captures| {
            let flags = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let recursive = flags.split_whitespace().any(|f| {
                (f.starts_with('-')
                    && !f.starts_with("--")
                    && f.to_lowercase().contains('r')
                    && !f.contains('='))
                    || f == "--recursive"
                    || f == "-R"
            });
            if recursive {
                format!("{}{}", &caps[0], GREP_EXCLUDES)
            } else {
                caps[0].to_string()
            }
        })
        .into_owned()
}

/// İstenen kabuk için (komut, kabuk mu) (`_command_argv`).
/// Çok satırlı cmd komutu `Err(MULTILINE_SHELL_MESSAGE)` döner.
pub fn command_argv(
    ctx: &ToolContext,
    command: &str,
    use_powershell: bool,
    use_python: bool,
) -> Result<Argv, String> {
    if use_python {
        return Ok(Argv::List(vec![
            ctx.python_exe.clone(),
            "-c".to_string(),
            command.to_string(),
        ]));
    }
    let mut use_powershell = use_powershell;
    if !use_powershell && cfg!(windows) {
        if ctx.windows_shell == "bash" {
            if let Some(bash) = &ctx.git_bash {
                return Ok(Argv::List(vec![
                    bash.to_string_lossy().into_owned(),
                    "-c".to_string(),
                    command.to_string(),
                ]));
            }
        }
        if ctx.windows_shell == "powershell" {
            use_powershell = true;
        }
    }
    if !use_powershell {
        if cfg!(windows) && command.trim().contains('\n') {
            for (i, re) in inline_python_re().iter().enumerate() {
                if let Some(m) = re.captures(command) {
                    let mut code = m
                        .name("code")
                        .map(|c| c.as_str().to_string())
                        .unwrap_or_default();
                    if i == 0 {
                        code = code.replace("\\\"", "\"");
                    }
                    return Ok(Argv::List(vec![
                        ctx.python_exe.clone(),
                        "-c".to_string(),
                        code,
                    ]));
                }
            }
            return Err(MULTILINE_SHELL_MESSAGE.to_string());
        }
        let command = if cfg!(windows) {
            pin_windows_filters(command)
        } else {
            command.to_string()
        };
        return Ok(Argv::Shell(command));
    }
    let exe = if cfg!(windows) { "powershell" } else { "pwsh" };
    let mut body = command.to_string();
    for (i, re) in powershell_wrapper_re().iter().enumerate() {
        if let Some(m) = re.captures(command) {
            body = m
                .name("body")
                .map(|c| c.as_str().to_string())
                .unwrap_or_default();
            if i == 0 {
                body = body.replace("\\\"", "\"");
            }
            break;
        }
    }
    let script = format!("[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; {body}");
    Ok(Argv::List(vec![
        exe.to_string(),
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-Command".to_string(),
        script,
    ]))
}

/// `run_command` zaman aşımını 1..600 aralığına sıkıştırır.
pub fn clamp_timeout(timeout_seconds: Option<i64>) -> i64 {
    let t = match timeout_seconds {
        Some(t) if t != 0 => t,
        _ => DEFAULT_COMMAND_TIMEOUT,
    };
    t.clamp(1, MAX_COMMAND_TIMEOUT)
}

/// `tool_run_command`: komutu çalıştırır, çıktıyı canlı akıtır, sonucu özetler.
pub fn tool_run_command(
    ctx: &ToolContext,
    command: &str,
    auto_approve: bool,
    timeout_seconds: Option<i64>,
    use_powershell: bool,
    use_python: bool,
) -> String {
    let timeout = clamp_timeout(timeout_seconds);
    if let Some(denied) = ctx.request_approval(auto_approve, command_request(command)) {
        return denied;
    }
    let unsealed = ctx.expand_secrets(command);
    let expanded = if use_powershell || use_python {
        unsealed
    } else {
        speed_grep(&unsealed)
    };
    let argv = match command_argv(ctx, &expanded, use_powershell, use_python) {
        Ok(a) => a,
        Err(e) => return format!("HATA: {e}"),
    };
    let mut cmd = argv.to_command();
    cmd.current_dir(ctx.root()).envs(ctx.secret_env());
    let outcome = match stream_with_tree_kill(
        &mut cmd,
        Some(Duration::from_secs(timeout as u64)),
        |tag, line| ctx.emit_output(tag, line),
    ) {
        Ok(o) => o,
        Err(e) => return format!("HATA: Komut çalıştırılamadı: {e}"),
    };
    if outcome.timed_out {
        return format!(
            "Çıkış Kodu: {TIMEOUT_EXIT_CODE}\nHATA: Komut {timeout} sn sonra zaman aşımına uğradı (süreç ağacı öldürüldü)."
        );
    }
    let out = outcome.stdout.trim();
    let err = outcome.stderr.trim();
    let mut lines = vec![format!("Çıkış Kodu: {}", outcome.code)];
    if !out.is_empty() {
        lines.push(format!(
            "STDOUT:\n{}",
            spill_long_output(out, Some(&ctx.root()), "cmd", command)
        ));
    }
    if !err.is_empty() {
        lines.push(format!("STDERR:\n{}", truncate_output(err)));
    }
    lines.join("\n")
}

/// `run_in_background=true`: komutu arka planda başlatır, kimliğini döner.
pub fn start_background_command(
    ctx: &ToolContext,
    command: &str,
    use_powershell: bool,
    use_python: bool,
    auto_approve: bool,
) -> String {
    if let Some(denied) = ctx.request_approval(auto_approve, background_request(command)) {
        return denied;
    }
    let unsealed = ctx.expand_secrets(command);
    let argv = match command_argv(ctx, &unsealed, use_powershell, use_python) {
        Ok(a) => a,
        Err(e) => return format!("HATA: {e}"),
    };
    let list: Option<Vec<String>> = match &argv {
        Argv::List(l) => Some(l.clone()),
        Argv::Shell(_) => None,
    };
    let mut manager = lock_manager(ctx);
    match manager.start(&unsealed, &ctx.root(), list.as_deref(), &ctx.secret_env()) {
        Ok(record) => format!(
            "BAŞARILI: Arka plan komutu başlatıldı (id: {}). Çıktı/durum için check_background, sonlandırmak için stop_background kullan.",
            record.id
        ),
        Err(e) => format!("HATA: Arka plan komutu başlatılamadı: {e}"),
    }
}

fn lock_manager(
    ctx: &ToolContext,
) -> std::sync::MutexGuard<'_, super::background::BackgroundManager> {
    match ctx.background.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn parse_wait(wait_seconds: Option<&Value>) -> i64 {
    let wait = match wait_seconds {
        None | Some(Value::Null) => CHECK_DEFAULT_WAIT,
        Some(Value::String(s)) if s.is_empty() => CHECK_DEFAULT_WAIT,
        Some(Value::String(s)) => s
            .trim()
            .parse::<f64>()
            .map(|f| f as i64)
            .unwrap_or(CHECK_DEFAULT_WAIT),
        Some(Value::Number(n)) => n.as_f64().map(|f| f as i64).unwrap_or(CHECK_DEFAULT_WAIT),
        Some(Value::Bool(b)) => i64::from(*b),
        Some(_) => CHECK_DEFAULT_WAIT,
    };
    wait.clamp(0, CHECK_MAX_WAIT)
}

/// `tool_check_background`: bitmesini en çok `wait_seconds` bekler, yeni çıktıyı döner.
pub fn tool_check_background(ctx: &ToolContext, id: &str, wait_seconds: Option<&Value>) -> String {
    let wait = parse_wait(wait_seconds);
    let known_exists = lock_manager(ctx).get(id).is_some();
    if known_exists && wait > 0 {
        let deadline = Instant::now() + Duration::from_secs(wait as u64);
        loop {
            let running = lock_manager(ctx).get(id).is_some_and(|r| r.running());
            if !running || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // Okuyucu iş parçacığının kuyruğu boşaltması için kısa pay.
        std::thread::sleep(Duration::from_millis(50));
    }
    let manager = lock_manager(ctx);
    let Some((output, status, code)) = manager.check(id) else {
        return unknown_id(&manager, id);
    };
    let mut output = output;
    if char_len(&output) > MAX_CHECK_CHARS {
        output = format!("{}\n[çıktı kırpıldı]", take_chars(&output, MAX_CHECK_CHARS));
    }
    let mut lines = vec![format!("Durum: {status}")];
    if let Some(code) = code {
        lines.push(format!("Çıkış Kodu: {code}"));
    }
    let shown = if output.trim().is_empty() {
        "(son okumadan bu yana yeni çıktı yok)".to_string()
    } else {
        output
    };
    lines.push(format!("Yeni Çıktı:\n{shown}"));
    lines.join("\n")
}

fn unknown_id(manager: &super::background::BackgroundManager, id: &str) -> String {
    let ids = manager.ids();
    let known = if ids.is_empty() {
        "(kayıtlı yok)".to_string()
    } else {
        ids.join(", ")
    };
    format!("HATA: Bilinmeyen arka plan id: '{id}'. Kayıtlı id'ler: {known}")
}

/// `tool_stop_background`: süreç ağacını öldürür.
pub fn tool_stop_background(ctx: &ToolContext, id: &str) -> String {
    let mut manager = lock_manager(ctx);
    if !manager.stop(id) {
        return unknown_id(&manager, id);
    }
    format!("BAŞARILI: Arka plan komutu '{id}' ve süreç ağacı sonlandırıldı.")
}

/// Dışa açık yardımcı: akış etiketi adı (`stdout`/`stderr`).
pub fn stream_name(tag: StreamTag) -> &'static str {
    tag.as_str()
}
