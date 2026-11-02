//! REPL ek komutları (Python `extras.py` handle_* ve `repl.py` `_cmd_*`):
//! /rol /commit /context /compact /rewind /doctor /keys /debug /memory
//! /language /scan /files /add /drop /resume ve `.evren/commands/*.md`.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use evren_agent::sessions::list_sessions;
use evren_agent::trace_report::{last_task_report, DebugReport};
use evren_agent::AgentSession;
use evren_api::{mask_key, ChatRequest, KeyState};
use evren_core::config::{get_evren_dir, save_api_keys};
use evren_core::memory::{self, Scope};
use evren_core::texts;
use evren_core::tools::file_ops::{resolve_workspace_path, rollback_last_backup};
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::commands::{set_dynamic_commands, COMMAND_ALIASES};
use crate::handlers::key_quota_lines;
use crate::ui::{self, print_error, print_info, print_success, print_warning, thousands};

fn headless() -> bool {
    std::env::var("EVREN_HEADLESS")
        .map(|v| v == "1")
        .unwrap_or(false)
        || !std::io::stdin().is_terminal()
}

/// `prompt_confirm(msg, default=False)`: y/yes/evet/e onaylar.
pub fn prompt_confirm(msg: &str) -> bool {
    prompt_confirm_default(msg, false)
}

/// `prompt_confirm(msg, default=...)`; boş yanıt varsayılanı seçer.
pub fn prompt_confirm_default(msg: &str, default_yes: bool) -> bool {
    if headless() {
        println!("{}", texts::confirm_headless(msg, default_yes));
        return default_yes;
    }
    print!("{}", texts::confirm_prompt(msg, default_yes));
    ui::flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!("{}", texts::confirm_no_input(default_yes));
            default_yes
        }
        Ok(_) => {
            let a = line.trim().to_lowercase();
            if a.is_empty() {
                return default_yes;
            }
            matches!(a.as_str(), "y" | "yes" | "evet" | "e")
        }
    }
}

/// `input(label)`; girdi yoksa `None`.
fn read_answer(label: &str) -> Option<String> {
    print!("{label}");
    ui::flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}

/// Python `round()` (yarımda çifte).
fn py_round(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 {
        (r - x.signum()) as i64
    } else {
        r as i64
    }
}

/// Python `str(float)`: tam sayılarda ".0" kalır.
fn py_float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// --------------------------------------------------------------------------- //
// git
// --------------------------------------------------------------------------- //

/// `_git`: çıkış kodu ve stdout (hata durumunda + stderr).
pub fn git(root: &Path, args: &[&str]) -> (i32, String) {
    match Command::new("git").args(args).current_dir(root).output() {
        Err(e) => (1, e.to_string()),
        Ok(out) => {
            let code = out.status.code().unwrap_or(1);
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            if code != 0 {
                text.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            (code, text)
        }
    }
}

// --------------------------------------------------------------------------- //
// /rol
// --------------------------------------------------------------------------- //

fn settings_path(root: &Path) -> PathBuf {
    get_evren_dir(root).join("settings.json")
}

fn load_settings(root: &Path) -> Map<String, Value> {
    std::fs::read_to_string(settings_path(root))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(t.trim_start_matches('\u{feff}')).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

fn save_settings(root: &Path, data: &Map<String, Value>) {
    let path = settings_path(root);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".into());
    let _ = std::fs::write(path, text);
}

pub fn role(session: &mut AgentSession, arg: &str) {
    let root = session.workspace_root().to_path_buf();
    let arg = arg.trim();
    if arg.is_empty() {
        let role = evren_agent::context::persistent_role(&root);
        if role.is_empty() {
            print_info("Kalıcı rol ayarlanmamış. Ayarlamak için: /rol <metin>");
        } else {
            print_info(&format!("Kalıcı rol: {role}"));
        }
        return;
    }
    let mut data = load_settings(&root);
    if matches!(arg.to_lowercase().as_str(), "temizle" | "clear" | "sil") {
        data.remove("role");
        save_settings(&root, &data);
        session.refresh_context();
        print_success("Kalıcı rol temizlendi.");
        return;
    }
    data.insert("role".into(), Value::from(arg));
    save_settings(&root, &data);
    session.refresh_context();
    print_success(&format!("Kalıcı rol ayarlandı: {arg}"));
}

// --------------------------------------------------------------------------- //
// Özel komutlar (.evren/commands/*.md)
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommand {
    pub body: String,
    pub desc: String,
    pub path: PathBuf,
}

pub fn custom_command_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.join(".evren").join("commands")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".evren").join("commands"));
    }
    dirs
}

fn custom_name_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^/[a-z][\w-]*$").ok())
        .as_ref()
}

/// `/ad` → komut; proje dosyaları kullanıcı düzeyindekileri ezer.
pub fn load_custom_commands_from(dirs: &[PathBuf]) -> BTreeMap<String, CustomCommand> {
    let mut found = BTreeMap::new();
    for dir in dirs.iter().rev() {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
            .collect();
        paths.sort();
        for path in paths {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let name = format!("/{stem}");
            let builtin = COMMAND_ALIASES.iter().any(|(c, _)| *c == name);
            if builtin || !custom_name_re().is_some_and(|re| re.is_match(&name)) {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let first = body
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim_matches(['#', ' ']).trim().to_string())
                .unwrap_or_default();
            let desc: String = first.chars().take(60).collect();
            found.insert(
                name,
                CustomCommand {
                    body,
                    desc: if desc.is_empty() {
                        "özel komut".into()
                    } else {
                        desc
                    },
                    path,
                },
            );
        }
    }
    found
}

pub fn refresh_custom_commands(root: &Path) -> BTreeMap<String, CustomCommand> {
    let cmds = load_custom_commands_from(&custom_command_dirs(root));
    set_dynamic_commands(cmds.keys());
    cmds
}

pub fn render_custom_command(entry: &CustomCommand, args: &str) -> String {
    if entry.body.contains("$ARGUMENTS") {
        return entry.body.replace("$ARGUMENTS", args);
    }
    if args.is_empty() {
        entry.body.clone()
    } else {
        format!("{}\n\n{args}", entry.body)
    }
}

// --------------------------------------------------------------------------- //
// /commit /review /init
// --------------------------------------------------------------------------- //

pub const REVIEW_PROMPT: &str =
    "ask: Çalışma dizinindeki değişiklikleri `git diff HEAD` ile incele ve kod incelemesi yap: \
hatalar, güvenlik, eksik testler, gereksiz karmaşıklık. Dosya:satır göstererek, önem sırasıyla, \
kısa yaz. Dosyaları değiştirme.";

pub const INIT_PROMPT: &str = "Bu projeyi kısaca incele (yapı, dil/çatı, test ve derleme komutları, kod stili) ve proje kökünde \
AGENTS.md dosyasını write_file ile oluştur: proje özeti, dizin yapısı, kurulum/test/çalıştırma \
komutları, kodlama kuralları ve ajan için uyarılar. 80 satırı geçme; sır yazma.";

pub async fn commit(session: &AgentSession) {
    let root = session.workspace_root().to_path_buf();
    if git(&root, &["rev-parse", "--is-inside-work-tree"]).0 != 0 {
        print_warning("Bu dizin bir git deposu değil.");
        return;
    }
    let (_, status) = git(&root, &["status", "--short"]);
    if status.trim().is_empty() {
        print_info("Commit edilecek değişiklik yok.");
        return;
    }
    let (_, staged) = git(&root, &["diff", "--cached", "--stat"]);
    let use_cached = !staged.trim().is_empty();
    let diff_args: &[&str] = if use_cached {
        &["diff", "--cached"]
    } else {
        &["diff", "HEAD"]
    };
    let (_, stat) = git(&root, &[diff_args, &["--stat"]].concat());
    let (_, diff) = git(&root, diff_args);
    let (_, log) = git(&root, &["log", "-5", "--format=%s"]);
    let diff: String = diff.chars().take(12000).collect();
    let prompt = format!(
        "Aşağıdaki git değişikliği için TEK bir commit mesajı yaz. Depodaki son commit \
         mesajlarının biçimini ve dilini izle. Yalnızca mesajı döndür (başlık + gerekirse \
         kısa gövde), açıklama veya tırnak ekleme.\n\n\
         SON 5 COMMIT:\n{}\n\nDEĞİŞİKLİK ÖZETİ:\n{}\n\n\
         DIFF (kırpılmış):\n{diff}",
        log.trim(),
        stat.trim()
    );
    let mut req = ChatRequest::new(vec![json!({"role": "user", "content": prompt})]);
    req.model = Some(session.model().to_string());
    req.max_tokens = Some(400);
    req.temperature = Some(0.2);
    req.thinking = Some(false);
    let response = match session.client().chat_complete(req).await {
        Ok(r) => r,
        Err(e) => {
            print_error(&format!("Commit mesajı üretilemedi: {e}"));
            return;
        }
    };
    let message = response
        .content()
        .unwrap_or_default()
        .trim()
        .trim_matches('`')
        .trim()
        .to_string();
    if message.is_empty() {
        print_error("Model boş commit mesajı döndürdü.");
        return;
    }
    print_info("Önerilen commit mesajı:");
    println!("{message}");
    let scope = if use_cached {
        "hazırlanmış (staged) değişiklikler"
    } else {
        "tüm değişiklikler (git add -A)"
    };
    if !prompt_confirm(&format!(
        "Commit edilsin mi? Kapsam: {scope}. Push yapılmaz."
    )) {
        print_info("Commit iptal edildi.");
        return;
    }
    if !use_cached {
        git(&root, &["add", "-A"]);
    }
    let (code, out) = git(&root, &["commit", "-m", &message]);
    let line = out.trim().lines().next().unwrap_or("commit").to_string();
    if code == 0 {
        print_success(&line);
    } else {
        print_error(&line);
    }
}

/// `/review`: incelenecek değişiklik yoksa `false`.
pub fn review_ready(root: &Path) -> bool {
    let (code, diff) = git(root, &["diff", "HEAD", "--stat"]);
    if code != 0 || diff.trim().is_empty() {
        print_info("İncelenecek git değişikliği yok.");
        return false;
    }
    true
}

/// `/init`: AGENTS.md varsa üzerine yazma onayı.
pub fn init_ready(root: &Path) -> bool {
    if root.join("AGENTS.md").exists()
        && !prompt_confirm("AGENTS.md zaten var. Üzerine yazılsın mı?")
    {
        print_info("/init iptal edildi.");
        return false;
    }
    true
}

// --------------------------------------------------------------------------- //
// /context /compact /rewind /doctor
// --------------------------------------------------------------------------- //

pub fn context(session: &AgentSession) {
    let tm = session.token_manager();
    let messages = session.messages();
    let role = |m: &Value| {
        m.get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let system: i64 = messages
        .iter()
        .filter(|m| role(m) == "system")
        .map(|m| tm.estimate_message(m))
        .sum();
    let schemas = evren_core::tools::registry::tools_for_mode(session.mode(), true, false);
    let tools = tm.estimate_text(&serde_json::to_string(&schemas).unwrap_or_default());
    let tool_msgs: Vec<&Value> = messages.iter().filter(|m| role(m) == "tool").collect();
    let tool_total: i64 = tool_msgs.iter().map(|m| tm.estimate_message(m)).sum();
    let other: i64 = messages
        .iter()
        .filter(|m| !matches!(role(m).as_str(), "system" | "tool"))
        .map(|m| tm.estimate_message(m))
        .sum();
    let total = system + tools + tool_total + other;
    let window = session.compaction_limit().max(1);
    print_info("Bağlam kırılımı (tahmini token):");
    println!("  • Sistem istemi        : {}", thousands(system));
    println!("  • Araç şemaları        : {}", thousands(tools));
    println!("  • Konuşma (kullanıcı/model): {}", thousands(other));
    println!(
        "  • Araç çıktıları       : {} ({} adet)",
        thousands(tool_total),
        tool_msgs.len()
    );
    println!(
        "  • Toplam               : {} / {} (%{})",
        thousands(total),
        thousands(window),
        py_round(100.0 * total as f64 / window as f64)
    );
    let mut biggest = tool_msgs.clone();
    biggest.sort_by_key(|m| std::cmp::Reverse(tm.estimate_message(m)));
    biggest.truncate(3);
    if !biggest.is_empty() {
        println!("  Son/en büyük araç çıktıları:");
        for m in biggest {
            let content = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                None | Some(Value::Null) => String::new(),
                Some(v) => v.to_string(),
            };
            let first: String = content
                .trim()
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(70)
                .collect();
            println!("    - {} tk: {first}", thousands(tm.estimate_message(m)));
        }
    }
}

pub fn compact(session: &mut AgentSession, arg: &str) {
    let (before, after) = session.compact_now(arg.trim());
    print_success(&format!(
        "Geçmiş sıkıştırıldı: {} -> {} token (tahmini).",
        thousands(before),
        thousands(after)
    ));
}

pub fn rewind(session: &AgentSession) {
    let root = session.workspace_root().to_path_buf();
    let mut files: Vec<String> = Vec::new();
    for rec in session.tool_trace() {
        if let Some(path) = rec.path.as_deref().filter(|p| !p.is_empty()) {
            if rec.ok && !files.iter().any(|f| f == path) {
                files.push(path.to_string());
            }
        }
    }
    if files.is_empty() {
        print_info("Son görevde değiştirilen dosya kaydı yok.");
        return;
    }
    print_info(&format!(
        "Son görevde değişen dosyalar: {}",
        files.join(", ")
    ));
    if !prompt_confirm("Bu dosyalar yedeklerinden geri yüklensin mi?") {
        return;
    }
    for rel in &files {
        let target = resolve_workspace_path(rel, &root);
        let (ok, msg) = rollback_last_backup(&target, &root);
        if ok {
            print_success(&msg);
            continue;
        }
        // Yedek yoksa (yedekler isteğe bağlı) izlenen dosyalar git ile döner.
        let tracked = git(&root, &["ls-files", "--error-unmatch", rel]).0 == 0;
        if tracked
            && prompt_confirm(&format!(
                "{rel} için yedek yok; git ile son commit haline dönülsün mü?"
            ))
        {
            let (code, out) = git(&root, &["checkout", "--", rel]);
            if code == 0 {
                print_success(&format!("{rel}: git ile geri alındı."));
            } else {
                print_error(&format!("{rel}: {}", out.trim()));
            }
        } else {
            print_warning(&format!("{rel}: geri alınamadı ({msg})"));
        }
    }
}

fn evren_on_path() -> Vec<String> {
    let out = if cfg!(windows) {
        Command::new("where").arg("evren").output()
    } else {
        Command::new("which").args(["-a", "evren"]).output()
    };
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

async fn key_quota_rows(session: &AgentSession) -> Vec<(String, bool, Map<String, Value>)> {
    let client = session.client();
    let keys = client.with_pool(|p| p.key_values());
    let current = client.current_key();
    let mut rows = Vec::new();
    for key in &keys {
        let data = client.get_quota(Some(key)).await;
        rows.push((mask_key(key), current.as_deref() == Some(key), data));
    }
    rows
}

pub async fn doctor(session: &AgentSession) {
    let root = session.workspace_root().to_path_buf();
    print_info("EVREN tanılama:");
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    println!("  • Çalıştırılabilir     : {exe}");
    let paths = evren_on_path();
    let warn = if paths.len() <= 1 {
        ""
    } else {
        "  (UYARI: birden fazla)"
    };
    println!("  • PATH'te evren        : {} adet{warn}", paths.len());
    for p in &paths {
        println!("      {p}");
    }
    println!("  • Sürüm                : {}", evren_core::VERSION);
    let client = session.client();
    if client.with_pool(|p| p.is_empty()) {
        println!("  • API erişimi          : anahtar yok, atlandı");
    } else {
        match client.head_base_url().await {
            Ok(code) => println!(
                "  • API erişimi          : {} -> HTTP {code}",
                client.base_url()
            ),
            Err(kind) => println!("  • API erişimi          : ulaşılamadı ({kind})"),
        }
        let rows = key_quota_rows(session).await;
        print_info(&format!("API Kota ({} anahtar):", rows.len()));
        for line in key_quota_lines(&rows) {
            println!("{line}");
        }
    }
    match evren_core::ssh_hosts::load_hosts(Some(&root)) {
        Ok(hosts) => println!("  • SSH host sayısı      : {}", hosts.len()),
        Err(_) => println!("  • SSH host sayısı      : okunamadı"),
    }
    let has_git = Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    println!(
        "  • git                  : {}",
        if has_git { "var" } else { "YOK" }
    );
}

// --------------------------------------------------------------------------- //
// /keys
// --------------------------------------------------------------------------- //

pub fn keys(session: &AgentSession, arg: &str) {
    let client = session.client();
    let config_path = session.config().legacy_config_path.clone();
    let (sub, sub_arg) = match arg.trim().split_once(char::is_whitespace) {
        Some((s, a)) => (s.to_lowercase(), a.trim().to_string()),
        None => (arg.trim().to_lowercase(), String::new()),
    };
    let save = |keys: Vec<String>| save_api_keys(&config_path, &keys);
    match sub.as_str() {
        "add" => {
            if sub_arg.is_empty() {
                print_warning("Kullanım: /keys add <api_anahtarı>");
            } else if client.with_pool(|p| p.add_key(&sub_arg)) {
                match save(client.with_pool(|p| p.key_values())) {
                    Ok(()) => print_success(&format!(
                        "Yeni API anahtarı eklendi ve kaydedildi: {}",
                        config_path.display()
                    )),
                    Err(e) => print_error(&e.to_string()),
                }
            } else {
                print_warning("Anahtar zaten havuzda veya geçersiz.");
            }
        }
        "remove" => {
            if sub_arg.is_empty() {
                print_warning("Kullanım: /keys remove <api_anahtarı>");
            } else if client.with_pool(|p| p.remove_key(&sub_arg)) {
                match save(client.with_pool(|p| p.key_values())) {
                    Ok(()) => print_success(&format!(
                        "API anahtarı çıkarıldı ve config güncellendi: {}",
                        config_path.display()
                    )),
                    Err(e) => print_error(&e.to_string()),
                }
            } else {
                print_warning("Anahtar havuzda bulunamadı.");
            }
        }
        "reset" => {
            client.with_pool(|p| p.reset_states());
            print_success("Tüm anahtar durumları sıfırlandı (READY).");
        }
        "next" | "switch" => {
            if client.with_pool(|p| p.len()) < 2 {
                print_warning("Havuzda geçilecek başka anahtar yok.");
                return;
            }
            match client.with_pool(|p| p.rotate()) {
                None => {
                    print_warning("Şu an kullanılabilir anahtar yok (hepsi beklemede/devre dışı).")
                }
                Some(key) => {
                    print_success(&format!("Sıradaki anahtara geçildi: {}", mask_key(&key)))
                }
            }
        }
        _ => {
            let lines = client.with_pool(|p| {
                if p.is_empty() {
                    return None;
                }
                let now = Instant::now();
                let current = p.current_key().map(str::to_string);
                let rows: Vec<String> = p
                    .keys()
                    .iter()
                    .enumerate()
                    .map(|(i, pk)| {
                        let label = match pk.state {
                            KeyState::Ready => "hazır",
                            KeyState::Cooling => "beklemede",
                            KeyState::Disabled => "devre dışı",
                        };
                        let extra = match (pk.state, &pk.last_error) {
                            (KeyState::Cooling, _) => {
                                format!(" (~{:.0} sn)", pk.next_available_in(now))
                            }
                            (KeyState::Disabled, Some(err)) if !err.is_empty() => {
                                format!(" ({err})")
                            }
                            _ => String::new(),
                        };
                        let marker = if current.as_deref() == Some(pk.key.as_str()) {
                            " ← aktif"
                        } else {
                            ""
                        };
                        format!("  {}. {:<24} [{label}]{extra}{marker}", i + 1, pk.masked())
                    })
                    .collect();
                Some((p.len(), rows))
            });
            match lines {
                None => print_info("Anahtar havuzu boş. Eklemek için: /keys add <anahtar>"),
                Some((n, rows)) => {
                    print_info(&format!("API Anahtar Havuzu ({n} anahtar):"));
                    for row in rows {
                        println!("{row}");
                    }
                    println!("\nKomutlar: /keys add <anahtar> | /keys remove <anahtar> | /keys next | /keys reset");
                }
            }
        }
    }
}

// --------------------------------------------------------------------------- //
// /debug /memory /language /scan /files /add /drop /resume /clear
// --------------------------------------------------------------------------- //

pub const DEBUG_USAGE: &str = "Kullanım: /debug [last] [N]  (N: sondan N. görev, varsayılan 1)";

/// `/debug [last] [N]` argümanı; geçersizse `None`.
pub fn parse_debug_arg(arg: &str) -> Option<usize> {
    let words: Vec<&str> = arg
        .split_whitespace()
        .filter(|w| w.to_lowercase() != "last")
        .collect();
    match words.first() {
        None => Some(1),
        Some(w) if !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()) => w.parse().ok(),
        Some(_) => None,
    }
}

pub fn debug(session: &AgentSession, arg: &str) {
    let Some(index) = parse_debug_arg(arg) else {
        print_warning(DEBUG_USAGE);
        return;
    };
    match last_task_report(
        session.workspace_root(),
        index,
        session.config().trace_enabled(),
    ) {
        DebugReport::Warning(m) => print_warning(&m),
        DebugReport::Info(m) => print_info(&m),
        DebugReport::Lines(lines) => {
            for line in lines {
                println!("{line}");
            }
        }
    }
}

pub fn memory(session: &mut AgentSession, arg: &str) {
    let root = session.workspace_root().to_path_buf();
    let mut scope = Scope::Project;
    let mut arg = arg.to_string();
    if arg.split_whitespace().any(|w| w == "--global") {
        scope = Scope::Global;
        arg = arg
            .split_whitespace()
            .filter(|w| *w != "--global")
            .collect::<Vec<_>>()
            .join(" ");
    }
    let parts: Vec<&str> = arg
        .splitn(3, char::is_whitespace)
        .filter(|p| !p.is_empty())
        .collect();
    let sub = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "set" => {
            if parts.len() < 3 {
                print_warning("Kullanım: /memory set <anahtar> <değer>");
            } else {
                let value = parts[2].trim();
                match memory::set_profile(parts[1], value, &root, scope) {
                    Ok(_) => {
                        print_success(&format!("Hafıza güncellendi: {} = {value}", parts[1]));
                        session.refresh_context();
                    }
                    Err(e) => print_error(&e),
                }
            }
        }
        "note" => {
            let text = arg
                .trim()
                .split_once(char::is_whitespace)
                .map(|(_, t)| t.trim().to_string())
                .unwrap_or_default();
            if text.is_empty() {
                print_warning("Kullanım: /memory note <metin>");
            } else {
                match memory::add_note(&text, &root, scope) {
                    Ok(_) => {
                        print_success("Not hafızaya eklendi.");
                        session.refresh_context();
                    }
                    Err(e) => print_error(&e),
                }
            }
        }
        _ => {
            let state = memory::load_memory(&root, scope);
            print_info(if scope == Scope::Global {
                "Genel Hafıza:"
            } else {
                "Proje Hafızası:"
            });
            match state.get("profile").and_then(Value::as_object) {
                Some(profile) if !profile.is_empty() => {
                    for (k, v) in profile {
                        println!("  • {k}: {}", value_text(v));
                    }
                }
                _ => println!("  (profil boş)"),
            }
            if let Some(notes) = state
                .get("notes")
                .and_then(Value::as_array)
                .filter(|n| !n.is_empty())
            {
                println!("  Notlar:");
                for n in &notes[notes.len().saturating_sub(10)..] {
                    let text = n.get("text").map(value_text).unwrap_or_default();
                    println!("    - {text}");
                }
            }
            println!(
                "\nKomutlar: /memory set <k> <v> | /memory note <metin>  (--global: tüm projeler)"
            );
        }
    }
}

pub fn language(session: &mut AgentSession, arg: &str) {
    let root = session.workspace_root().to_path_buf();
    if arg.is_empty() {
        let current = evren_core::language::get_language(&root);
        let current = if current.is_empty() {
            "(ayarlanmamış)".to_string()
        } else {
            current
        };
        print_info(&format!("Çıktı dili: {current}"));
        println!("Değiştirmek için: /language <tr|en|de|...>");
        return;
    }
    match evren_core::language::set_language(arg, &root) {
        Err(e) => print_error(&e),
        Ok(canonical) => {
            print_success(&format!("Çıktı dili ayarlandı: {canonical}"));
            session.refresh_context();
        }
    }
}

pub fn scan(workspace: &Path, arg: &str) {
    let target = if arg.is_empty() {
        workspace.to_path_buf()
    } else {
        PathBuf::from(arg)
    };
    let tree = evren_core::project_scan::generate_tree(&target, 3);
    let stats = evren_core::project_scan::scan_workspace(&target, 150, 250.0);
    println!("{tree}");
    print_info(&format!(
        "Toplam {} dosya ({} KB).",
        stats.total_files,
        py_float(stats.total_size_kb)
    ));
}

pub fn files(session: &AgentSession) {
    let files = session.context_files();
    if files.is_empty() {
        print_info("Bağlamda dosya yok. Eklemek için: /add <dosya_yolu>");
    } else {
        println!("\nAktif Bağlam Dosyaları:");
        for f in files {
            println!("  • {f}");
        }
        println!();
    }
}

pub fn add(session: &mut AgentSession, arg: &str) {
    if arg.is_empty() {
        print_warning("Kullanım: /add <dosya_yolu>");
    } else {
        session.add_context_file(arg);
        print_success(&format!("'{arg}' bağlama eklendi."));
    }
}

pub fn drop(session: &mut AgentSession, arg: &str) {
    if arg.is_empty() {
        print_warning("Kullanım: /drop <dosya_yolu>");
    } else {
        session.remove_context_file(arg);
        print_info(&format!("'{arg}' bağlamdan çıkarıldı."));
    }
}

fn session_title(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("title").map(value_text))
        .unwrap_or_default()
}

/// `/resume`: kayıtlı oturumları listeler, seçileni yükler.
pub fn resume(session: &mut AgentSession) {
    let items = list_sessions(session.workspace_root(), 10);
    if items.is_empty() {
        print_warning("Kayıtlı oturum yok.");
        return;
    }
    for (i, p) in items.iter().enumerate() {
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!("  {}. {stem}  {}", i + 1, session_title(p));
    }
    if headless() {
        return;
    }
    let Some(ans) = read_answer("Numara (boş=iptal): ") else {
        return;
    };
    let Ok(n) = ans.parse::<usize>() else {
        return;
    };
    if n >= 1 && n <= items.len() {
        if session.load_session(&items[n - 1]) {
            print_info("Oturum yüklendi; kaldığınız yerden devam edebilirsiniz.");
        } else {
            print_warning("Oturum okunamadı.");
        }
    }
}

/// `workflow_state.clear_checkpoint`: `.evren/checkpoint.json` silinir.
pub fn clear_checkpoint(root: &Path) {
    let _ = std::fs::remove_file(get_evren_dir(root).join("checkpoint.json"));
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn custom_commands_project_wins_and_builtin_skipped() {
        let proj = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            proj.path().join("Deploy.md"),
            "# Yayına al\nAdımlar: $ARGUMENTS",
        )
        .unwrap();
        std::fs::write(home.path().join("deploy.md"), "kullanıcı sürümü").unwrap();
        std::fs::write(home.path().join("lint.md"), "\n\n").unwrap();
        std::fs::write(proj.path().join("help.md"), "yerleşik").unwrap();
        std::fs::write(proj.path().join("9bad.md"), "geçersiz ad").unwrap();
        std::fs::write(proj.path().join("notes.txt"), "md değil").unwrap();
        let cmds = load_custom_commands_from(&[proj.path().into(), home.path().into()]);
        assert_eq!(cmds.keys().collect::<Vec<_>>(), vec!["/deploy", "/lint"]);
        let deploy = &cmds["/deploy"];
        assert_eq!(deploy.desc, "Yayına al");
        assert_eq!(cmds["/lint"].desc, "özel komut");
        assert_eq!(
            render_custom_command(deploy, "prod"),
            "# Yayına al\nAdımlar: prod"
        );
        assert_eq!(render_custom_command(&cmds["/lint"], "x"), "\n\n\n\nx");
        assert_eq!(render_custom_command(&cmds["/lint"], ""), "\n\n");
    }

    #[test]
    fn dynamic_commands_are_commands() {
        use crate::commands::{is_command, is_unknown_command};
        set_dynamic_commands(["/Deploy"]);
        assert!(is_command("/deploy prod"));
        assert!(!is_unknown_command("/deploy"));
        set_dynamic_commands(Vec::<String>::new());
        assert!(!is_command("/deploy prod"));
        assert!(is_unknown_command("/deploy"));
    }

    #[test]
    fn debug_args() {
        assert_eq!(parse_debug_arg(""), Some(1));
        assert_eq!(parse_debug_arg("last"), Some(1));
        assert_eq!(parse_debug_arg("last 3"), Some(3));
        assert_eq!(parse_debug_arg("2"), Some(2));
        assert_eq!(parse_debug_arg("0"), Some(0));
        assert_eq!(parse_debug_arg("x"), None);
        assert_eq!(parse_debug_arg("-1"), None);
    }

    #[test]
    fn number_formats() {
        assert_eq!(py_float(0.0), "0.0");
        assert_eq!(py_float(12.3), "12.3");
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(3.5), 4);
    }

    #[test]
    fn role_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = load_settings(dir.path());
        assert!(data.is_empty());
        data.insert("role".into(), Value::from("kıdemli geliştirici"));
        save_settings(dir.path(), &data);
        assert_eq!(
            evren_agent::context::persistent_role(dir.path()),
            "kıdemli geliştirici"
        );
        clear_checkpoint(dir.path());
    }
}
