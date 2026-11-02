//! Araç kapısı, kip denetimi ve tekrar koruması
//! (Python `tool_gateway.py` + `agent.py` koruma bölümleri).

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::OnceLock;

use evren_core::tools::registry::{read_only_tools, repeat_exempt_tools, write_tools};
use evren_core::tools::Mode;
use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde_json::Value;

use crate::compaction::tail_chars;
use crate::todos::TodoStore;
use crate::usage_log::py_dumps;

// ---------- sabitler ----------

pub const MAX_STALLS: u32 = 4;
pub const MAX_API_RETRIES: u32 = 12;
pub const LOOP_BLOCK_LIMIT: u32 = 3;
pub const MAX_LOOP_RELEASES: u32 = 2;
pub const FAMILY_RUN_LIMIT: u32 = 4;
pub const PROBE_NUDGE_AT: u32 = 4;
pub const DELEGATE_DEFAULT_STEPS: u32 = 6;
pub const DELEGATE_MAX_STEPS: u32 = 12;
pub const PARALLEL_SAFE: [&str; 6] = [
    "view_file",
    "find_files",
    "list_directory",
    "web_fetch",
    "ssh_read_file",
    "search_code",
];
pub const TASK_NOISE_PREFIXES: [&str; 3] = ["[EVREN]", "Önceki yanıt token", "[Önceki"];
const CODE_EXTS: [&str; 10] = [
    ".py", ".cs", ".ts", ".tsx", ".js", ".jsx", ".go", ".rs", ".java", ".kt",
];

macro_rules! re {
    ($name:ident, $pat:expr) => {
        pub fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| {
                #[allow(clippy::unwrap_used)]
                Regex::new($pat).unwrap()
            })
        }
    };
}

macro_rules! fre {
    ($name:ident, $pat:expr) => {
        pub fn $name() -> &'static FancyRegex {
            static RE: OnceLock<FancyRegex> = OnceLock::new();
            RE.get_or_init(|| {
                #[allow(clippy::unwrap_used)]
                FancyRegex::new($pat).unwrap()
            })
        }
    };
}

re!(
    listing_re,
    r"(?i)\b(listele|göster|goster|yazdır|getir|list|show)"
);
re!(
    analysis_re,
    r"(?i)(analiz|karşılaştır|kontrol|neden|hangi|bul\b|süz|filtre|sadece|yalnızca|olmayan|eksik|hatalı)"
);
re!(
    plan_intent_re,
    r"(?i)\b(planla\w*|plan\s+(yap|çıkar|hazırla|oluştur)\w*|önce\s+plan)"
);
re!(
    no_plan_re,
    r"(?i)\bplan\s+(yazma|yapma|çıkarma|sunma)\b|\bplanlama\s+yapma\b"
);
fre!(
    plan_path_re,
    r"(?<![\w/.-])((?:[\w.-]+/)+[\w.-]+\.(?:cs|csproj|py|ts|tsx|js|jsx|json|xslt|xml|sql|java|kt|go|rs|yml|yaml|md))\b"
);
re!(
    mutating_command_re,
    r"(?ix)
    \bgit\s+(?:-C\s+\S+\s+)?(?:checkout|switch|pull|merge|rebase|reset|apply|am|stash|cherry-pick|revert|commit|restore|clean|mv|rm|fetch)\b
    |\b(?:pip|pip3|npm|pnpm|yarn|dotnet|cargo|go|poetry|uv)\s+(?:install|add|remove|uninstall|restore|update|upgrade|new|publish|tool)\b
    |(?:^|[\s;&|(])(?:rm|del|erase|rmdir|rd|mv|move|cp|copy|xcopy|robocopy|mkdir|md|ren|rename|touch|truncate|tee)\b
    |\bsed\s+-i\b
    |\b(?:Set-Content|Add-Content|Out-File|New-Item|Remove-Item|Copy-Item|Move-Item|Rename-Item)\b"
);
fre!(
    redirect_re,
    r"(?<![0-9&<>])>>?\s*(?!&|nul\b|NUL\b|/dev/null|\$null)\S"
);
fre!(
    handoff_re,
    r"(?i)(tek\s+somut\s+sonraki\s+ad[ıi]m|kalan\s+i[şs]\s*[:*]+\s*(?!\**\s*yok\b)\S|d[üu]zenleme\s+yap[ıi]lamad[ıi])"
);
fre!(
    asks_approval_re,
    r"(?i)(ister\s+mis(in|iniz)|uygulayay[ıi]m\s+m[ıi]|yapay[ıi]m\s+m[ıi]|onay[ıi]n[ıi]?z?\s+(al[ıi]yorum|bekliyorum)|onay\s+bekliyorum|onayl[ıi]yor\s+musun|onaylarsan|onay\s+verirsen|onay\s+verir\s+misin|devam\s+edeyim\s+mi|ba[şs]lay[ae]y[ıi]m\s+m[ıi]|ba[şs]lat[ae]y[ıi]m\s+m[ıi]|uygulamam[ıi]\s+ister|s[ıi]radaki\s+(tek\s+)?(somut\s+)?ad[ıi]m|yap[ıi]lacak(lar)?\s*[:\n]|sonraki\s+ad[ıi]m(lar)?\s*[:\n*]|kalan\s+i[şs]\s*[:*]+\s*(?!\**\s*yok\b)\S|d[üu]zenleme\s+yap[ıi]lamad[ıi])"
);
fre!(
    imperative_re,
    r"(?i)\b(düzelt|ekle|uygula|güncelle|değiştir|oluştur|kaldır|yaz|düzenle|yap|tamamla|dönüştür|kopyala|taşı|çevir|geliştir|bağla|doğrudan)(?!me\b|ma\b|meyin|mayın|madan|meden)\w*"
);
re!(
    output_noise_re,
    r"(?i)\d+(?:\.\d+)?\s*(?:s|ms|sn|saniye)\b|\d{1,2}:\d{2}(?::\d{2})?|0x[0-9a-fA-F]+|tmp\w+"
);
re!(
    test_cmd_re,
    r"(?i)\b(pytest|unittest|dotnet\s+(test|build)|npm\s+(run\s+)?(test|build)|pnpm\s+(run\s+)?test|yarn\s+test|cargo\s+(test|build|check)|go\s+(test|build|vet)|mvn\s+(test|verify)|gradle\w*\s+(test|build)|tsc|compileall|py_compile)\b"
);
re!(quoted_re, r#""[^"]*"|'[^']*'"#);
re!(chain_sep_re, r"&&|\|\||;|\|");
re!(family_split_re, r"\b(?:do|&&|;)\s+");
re!(family_skip_re, r"^\s*(cd|export|set)\b");
re!(family_loop_re, r"^\s*(for|while)\b");
re!(host_re, r#"https?://([^/\s"']+)"#);

pub fn fancy_match(re: &FancyRegex, text: &str) -> bool {
    re.is_match(text).unwrap_or(false)
}

pub fn command_may_modify_files(command: &str) -> bool {
    mutating_command_re().is_match(command) || fancy_match(redirect_re(), command)
}

pub fn is_listing_task(text: &str) -> bool {
    listing_re().is_match(text) && !analysis_re().is_match(text)
}

/// `EVREN_STALL_TOKEN_CAP`; boş/geçersiz ya da 1024..=32768 dışı → 16384.
pub fn stall_token_cap() -> u32 {
    std::env::var("EVREN_STALL_TOKEN_CAP")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| (1024..=32768).contains(v))
        .unwrap_or(16384)
}

fn call_arguments_complete(call: &Value) -> bool {
    match call.get("function").and_then(|f| f.get("arguments")) {
        Some(Value::Object(_)) => true,
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).is_ok_and(|v| v.is_object()),
        _ => false,
    }
}

/// Boş liste → false; her çağrının argümanı JSON nesnesi olmalı.
pub fn tool_arguments_complete(calls: &[Value]) -> bool {
    !calls.is_empty() && calls.iter().all(call_arguments_complete)
}

/// Tamamlanmamış ilk çağrının adı ya da boş dize.
pub fn cut_tool_call_name(calls: &[Value]) -> String {
    calls
        .iter()
        .find(|c| !call_arguments_complete(c))
        .and_then(|c| c.get("function"))
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub fn repeat_exempt() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        let mut s = repeat_exempt_tools();
        s.insert("stop_background");
        s
    })
}

// ---------- tool_gateway ----------

const READ_ONLY_COMMAND_VERBS: [&str; 52] = [
    "ls",
    "dir",
    "cat",
    "type",
    "head",
    "tail",
    "less",
    "more",
    "find",
    "findstr",
    "grep",
    "rg",
    "select-string",
    "git",
    "pwd",
    "cd",
    "echo",
    "whoami",
    "hostname",
    "python",
    "python3",
    "py",
    "wc",
    "sort",
    "uniq",
    "diff",
    "tree",
    "get-childitem",
    "get-content",
    "get-item",
    "test-path",
    "where",
    "which",
    "stat",
    "file",
    "uptime",
    "df",
    "free",
    "ps",
    "top",
    "uname",
    "id",
    "date",
    "env",
    "netstat",
    "ss",
    "ip",
    "ifconfig",
    "journalctl",
    "nproc",
    "lsblk",
    "mount",
];
const READ_ONLY_GIT_SUBCOMMANDS: [&str; 16] = [
    "status",
    "log",
    "diff",
    "show",
    "branch",
    "remote",
    "describe",
    "rev-parse",
    "ls-files",
    "blame",
    "shortlog",
    "tag",
    "grep",
    "cat-file",
    "ls-tree",
    "merge-base", // "reflog" aşağıda
];
const SHELL_WRITE_MARKERS: [&str; 8] = [">", ">>", "|", "&&", "||", ";", "`", "$("];
const SUBSHELL_MARKERS: [&str; 3] = ["`", "$(", "<("];
const NEVER_READ_ONLY_MARKERS: [&str; 2] = [">", "\n"];

fn git_write_flags(sub: &str) -> &'static [&'static str] {
    match sub {
        "branch" => &[
            "-d",
            "-D",
            "-m",
            "-M",
            "-c",
            "-C",
            "-f",
            "--delete",
            "--move",
            "--copy",
            "--force",
            "-u",
            "--set-upstream-to",
            "--unset-upstream",
            "--edit-description",
        ],
        "tag" => &[
            "-d",
            "--delete",
            "-a",
            "-s",
            "-u",
            "-f",
            "--force",
            "-m",
            "-F",
            "--annotate",
            "--sign",
        ],
        "reflog" => &["expire", "delete"],
        _ => &[],
    }
}

fn git_list_flags(sub: &str) -> &'static [&'static str] {
    match sub {
        "branch" => &[
            "-a",
            "-r",
            "-v",
            "-vv",
            "--all",
            "--remotes",
            "--list",
            "-l",
            "--contains",
            "--merged",
            "--no-merged",
            "--show-current",
            "--points-at",
            "--sort",
            "--format",
            "--no-color",
            "--color",
        ],
        "tag" => &[
            "-l",
            "--list",
            "-n",
            "--contains",
            "--merged",
            "--no-merged",
            "--points-at",
            "--sort",
            "--format",
        ],
        _ => &[],
    }
}

fn is_read_only_git_sub(sub: &str) -> bool {
    READ_ONLY_GIT_SUBCOMMANDS.contains(&sub) || sub == "reflog"
}

/// `shlex.split(posix=False)`: tırnaklar korunur, tırnak dışı boşlukta bölünür.
fn shell_split(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in command.chars() {
        match quote {
            Some(q) => {
                cur.push(ch);
                if ch == q {
                    quote = None;
                }
            }
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                cur.push(ch);
            }
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    parts.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if quote.is_some() {
        // Kapanmamış tırnak: Python ValueError → düz boşluk bölmesi.
        return command.split_whitespace().map(str::to_string).collect();
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn strip_quotes(tok: &str) -> &str {
    tok.trim_matches(|c| c == '"' || c == '\'')
}

/// Tırnak dışındaki ayırıcılarda böler; boş parçalar atılır.
fn split_chain(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            cur.push(ch);
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            cur.push(ch);
            i += 1;
            continue;
        }
        let rest: String = chars[i..].iter().take(2).collect();
        if let Some(m) = chain_sep_re().find(&rest).filter(|m| m.start() == 0) {
            parts.push(std::mem::take(&mut cur));
            i += m.as_str().chars().count();
            continue;
        }
        cur.push(ch);
        i += 1;
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn first_token(parts: &[String]) -> String {
    let Some(first) = parts.first() else {
        return String::new();
    };
    let tok = strip_quotes(first).replace('\\', "/");
    let tok = tok.rsplit('/').next().unwrap_or("");
    let tok = tok.to_lowercase();
    tok.strip_suffix(".exe").unwrap_or(&tok).to_string()
}

fn second_token(parts: &[String]) -> String {
    parts
        .get(1)
        .map(|p| strip_quotes(p).to_lowercase())
        .unwrap_or_default()
}

fn git_subcommand(parts: &[String]) -> (String, Vec<String>) {
    let mut i = 1;
    while i < parts.len() {
        let tok = parts[i].as_str();
        if ["-C", "-c", "--git-dir", "--work-tree"].contains(&tok) {
            i += 2;
            continue;
        }
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        let rest = parts[i + 1..]
            .iter()
            .map(|p| strip_quotes(p).to_string())
            .collect();
        return (tok.to_lowercase(), rest);
    }
    (String::new(), Vec::new())
}

fn git_listing_only(sub: &str, args: &[String]) -> String {
    if args.iter().any(|a| a.starts_with("--output")) {
        return format!("'git {sub} --output' dosya yazar; ask/plan modunda kullanılamaz.");
    }
    let write_flags = git_write_flags(sub);
    if !write_flags.is_empty()
        && args
            .iter()
            .any(|a| write_flags.contains(&a.split('=').next().unwrap_or("")))
    {
        return format!(
            "'git {sub}' bu bayrakla değişiklik yapar; ask/plan modunda yalnız listeleme."
        );
    }
    if sub == "branch" || sub == "tag" {
        let list_flags = git_list_flags(sub);
        let listing = args
            .iter()
            .any(|a| list_flags.contains(&a.split('=').next().unwrap_or("")));
        let positional = args.iter().any(|a| !a.starts_with('-'));
        if positional && !listing {
            return format!(
                "'git {sub} <ad>' yeni ref oluşturur; ask/plan modunda yalnız listeleme."
            );
        }
    }
    if sub == "remote"
        && args
            .first()
            .is_some_and(|a| !["-v", "--verbose", "show", "get-url"].contains(&a.as_str()))
    {
        return "'git remote' yalnız listeleme (-v, show, get-url) ile kullanılabilir.".into();
    }
    String::new()
}

/// Komut ask/plan kipinde salt-okuma sayılır mı: `(evet, neden)`.
pub fn is_read_only_command(command: &str) -> (bool, String) {
    if command.trim().is_empty() {
        return (false, "Boş komut.".into());
    }
    for marker in SUBSHELL_MARKERS {
        if command.contains(marker) {
            return (
                false,
                format!("Komut '{marker}' içeriyor; ask/plan modunda alt kabuk çalıştırılamaz."),
            );
        }
    }
    let bare = quoted_re().replace_all(command, "").to_string();
    if !NEVER_READ_ONLY_MARKERS.iter().any(|m| bare.contains(m)) {
        let parts = split_chain(command);
        if parts.len() > 1 {
            for part in &parts {
                let (ok, why) = is_read_only_command(part);
                if !ok {
                    return (false, why);
                }
            }
            return (true, String::new());
        }
    }
    for marker in SHELL_WRITE_MARKERS {
        if bare.contains(marker) {
            return (
                false,
                format!(
                    "Komut '{marker}' içeriyor; ask/plan modunda yalnızca basit salt-okuma komutlarına izin verilir."
                ),
            );
        }
    }
    let parts = shell_split(command);
    let verb = first_token(&parts);
    if !READ_ONLY_COMMAND_VERBS.contains(&verb.as_str()) {
        return (
            false,
            format!("'{verb}' komutu salt-okuma listesinde değil."),
        );
    }
    if verb == "git" {
        let (sub, args) = git_subcommand(&parts);
        if !is_read_only_git_sub(&sub) {
            return (false, format!("'git {sub}' salt-okuma alt komutu değil."));
        }
        let why = git_listing_only(&sub, &args);
        if !why.is_empty() {
            return (false, why);
        }
    }
    if ["python", "python3", "py"].contains(&verb.as_str()) {
        let second = second_token(&parts);
        if !["--version", "-v", "-c"].contains(&second.as_str()) {
            return (
                false,
                "ask/plan modunda python yalnızca --version veya -c ile sınırlıdır.".into(),
            );
        }
    }
    (true, String::new())
}

fn arg_bool(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

pub fn arg_str<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Kip denetimi: `(izinli, neden)`.
pub fn check_tool_allowed(tool: &str, args: &Value, mode: Mode) -> (bool, String) {
    if mode == Mode::Normal {
        return (true, String::new());
    }
    if tool == "delegate" {
        return (
            false,
            "'delegate' yalnızca normal modda kullanılabilir.".into(),
        );
    }
    if read_only_tools().contains(tool) {
        return (true, String::new());
    }
    let label = match mode {
        Mode::Ask => "ask",
        _ => "plan",
    };
    if write_tools().contains(tool) {
        return (
            false,
            match mode {
                Mode::Ask => format!("'{tool}' ask modunda devre dışı (salt-okuma). Yazmak için normal moda geçin."),
                _ => format!("'{tool}' plan modunda devre dışı (kaynak dosyalara dokunulmaz). Uygulamak için normal moda geçin."),
            },
        );
    }
    if tool == "create_plan" {
        return match mode {
            Mode::Ask => (
                false,
                "'create_plan' ask modunda devre dışı. Plan moduna geçin.".into(),
            ),
            _ => (true, String::new()),
        };
    }
    if tool == "run_command" || tool == "ssh_run" {
        if tool == "run_command" && arg_bool(args, "run_in_background") {
            return (
                false,
                format!("{label} modunda arka plan komutu başlatılamaz (run_in_background). Normal moda geçin."),
            );
        }
        let (ok, reason) = is_read_only_command(arg_str(args, "command"));
        if !ok {
            return (false, format!("{label} modunda komut reddedildi: {reason}"));
        }
        return (true, String::new());
    }
    (false, format!("'{tool}' {label} modunda izinli değil."))
}

// ---------- tekrar koruması ----------

#[derive(Debug, Default)]
pub struct RepeatGuard {
    call_epochs: HashMap<String, u32>,
    output_seen: HashMap<(String, u64), u32>,
    pub write_epoch: u32,
    pub loop_blocks: u32,
    pub loop_releases: u32,
    pub loop_exhausted: bool,
    pub approval_nudges: u32,
    pub todo_nudges: u32,
    pub code_written: bool,
    pub verified_since_write: bool,
    pub verify_nudged: bool,
    pub verify_note: String,
    pub approval_nudged: bool,
    pub idle_steps: u32,
    pub idle_epoch: u32,
    family_runs: HashMap<String, (u32, u32)>,
    probe_runs: HashMap<u32, u32>,
    pub probe_note: String,
    /// Tekrar kesici: görev boyunca her imzanın sayısı (yazmayla sıfırlanmaz).
    sig_counts: HashMap<String, u32>,
    sig_last_out: HashMap<String, u64>,
    pub role_test_runs: u32,
    pub stuck_reason: String,
    pub repeat_blocked_count: u32,
    /// Rol profilinin (block, stuck) sınırları ve rapor dosyası; görevler arası korunur.
    role_limits: Option<(u32, u32)>,
    role_report: String,
}

/// `fn_name + ":" + json.dumps(args, sort_keys=True)`.
pub fn call_signature(fn_name: &str, args: &Value) -> String {
    format!("{fn_name}:{}", py_dumps(&crate::trace::sort_keys(args)))
}

/// `python -m pytest`, `dotnet test`, `curl host`...: araç, bayrakları değil.
pub fn command_family(fn_name: &str, args: &Value) -> String {
    if fn_name != "run_command" && fn_name != "ssh_run" {
        return String::new();
    }
    let command = arg_str(args, "command");
    let parts: Vec<&str> = family_split_re()
        .split(command)
        .filter(|c| !c.trim().is_empty() && !family_skip_re().is_match(c))
        .collect();
    let mut probe = parts.first().copied().unwrap_or(command);
    if family_loop_re().is_match(probe) && parts.len() > 1 {
        probe = parts[1];
    }
    let words: Vec<&str> = probe
        .split_whitespace()
        .filter(|w| !w.starts_with('-'))
        .collect();
    let Some(first) = words.first() else {
        return String::new();
    };
    let head = first.to_lowercase();
    if ["curl", "wget", "http", "invoke-webrequest", "irm", "iwr"].contains(&head.as_str()) {
        return match host_re().captures(command) {
            Some(c) => format!("{head} {}", &c[1]),
            None => head,
        };
    }
    if ["python", "python3", "py"].contains(&head.as_str()) {
        let raw: Vec<&str> = command.split_whitespace().collect();
        if raw.len() > 2 {
            if raw[1] == "-m" {
                return format!("python -m {}", raw[2]);
            }
            return String::new();
        }
    }
    if ["dotnet", "npm", "pnpm", "yarn", "go", "cargo", "pytest"].contains(&head.as_str()) {
        return words
            .iter()
            .take(2)
            .map(|w| w.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
    }
    String::new()
}

impl RepeatGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        let limits = self.role_limits;
        let report = std::mem::take(&mut self.role_report);
        *self = Self::default();
        self.role_limits = limits;
        self.role_report = report;
    }

    /// Rol profili: tekrar sınırları (block, stuck) ve rapor dosyası.
    pub fn set_role_limits(&mut self, limits: Option<(u32, u32)>, report: &str) {
        self.role_limits = limits;
        self.role_report = report.to_string();
    }

    /// (block, stuck): rol profilininki, yoksa `EVREN_REPEAT_LIMITS` (varsayılan 5,8).
    fn repeat_limits(&self) -> (u32, u32) {
        if let Some(limits) = self.role_limits {
            return limits;
        }
        let raw = std::env::var("EVREN_REPEAT_LIMITS")
            .unwrap_or_default()
            .replace(' ', "");
        let raw = if raw.is_empty() {
            "5,8".to_string()
        } else {
            raw
        };
        let parsed = raw
            .split_once(',')
            .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)));
        let (block, stuck) = parsed.unwrap_or((5, 8));
        let block = block.max(2);
        (block, stuck.max(block))
    }

    /// Bir imzayı görev boyunca sayar; `block`tan itibaren reddeder, `stuck`ta görevi durdurur.
    fn repeat_breaker(&mut self, signature: &str) -> String {
        let count = self.sig_counts.get(signature).copied().unwrap_or(0) + 1;
        self.sig_counts.insert(signature.to_string(), count);
        let (block, stuck) = self.repeat_limits();
        if count < block {
            return String::new();
        }
        self.repeat_blocked_count += 1;
        self.loop_blocks += 1;
        if count >= stuck {
            let short: String = signature.chars().take(160).collect();
            self.stuck_reason = format!("aynı çağrı bu görevde {count} kez geldi: {short}");
            return format!(
                "TEKRAR ENGELLENDİ (repeat_blocked): Bu çağrı bu görevde {count}. kez geldi; görev outcome=stuck ile durduruluyor."
            );
        }
        format!(
            "TEKRAR ENGELLENDİ (repeat_blocked): Bu çağrı bu görevde {count}. kez geldi ve sonucu çalıştırılmadı. Aynı yaklaşım sonuç vermiyor: hipotezini değiştir (farklı dosya, farklı ölçüm, farklı düzeltme) ya da engeli ve kanıtını yazıp 'blocked' raporla. {stuck}. tekrarda görev durdurulur."
        )
    }

    /// Aynı çağrının önceki çalışmasından farklı çıktı sayımı sıfırlar (ilerleme).
    fn note_breaker_output(&mut self, fn_name: &str, args: &Value, result: &str) {
        if repeat_exempt().contains(fn_name) {
            return;
        }
        let signature = call_signature(fn_name, args);
        let mut hasher = DefaultHasher::new();
        output_noise_re().replace_all(result, "").hash(&mut hasher);
        let digest = hasher.finish();
        let previous = self.sig_last_out.insert(signature.clone(), digest);
        if previous.is_some_and(|p| p != digest) {
            self.sig_counts.insert(signature, 1);
        }
    }

    /// Rolün `max_test_runs` kotası; aşıldıysa ret gerekçesi.
    pub fn role_test_quota(
        &mut self,
        role: &str,
        limit: Option<u32>,
        fn_name: &str,
        args: &Value,
    ) -> String {
        let Some(limit) = limit else {
            return String::new();
        };
        if fn_name != "run_command" && fn_name != "ssh_run" {
            return String::new();
        }
        if !test_cmd_re().is_match(arg_str(args, "command")) {
            return String::new();
        }
        if self.role_test_runs >= limit {
            let report = if self.role_report.is_empty() {
                "raporunu"
            } else {
                self.role_report.as_str()
            };
            return format!(
                "'{role}' rolü bu görevde test/derleme komutunu {limit} kez koştu (sınır). Kalan doğrulama orkestratörün işi: {report} dosyasına yapılanı ve son test sonucunu yaz, bitir."
            );
        }
        self.role_test_runs += 1;
        String::new()
    }

    /// Bu görevde `prefix` ile başlayan bir araç imzası görüldü mü (ör. `create_plan:`).
    pub fn has_call_prefix(&self, prefix: &str) -> bool {
        self.call_epochs.keys().any(|k| k.starts_with(prefix))
    }

    /// Çıktıları artık geçmişte olmayabilecek tekrar belleğini atar.
    pub fn forget_seen_calls(&mut self) {
        self.call_epochs.clear();
        self.output_seen.clear();
        self.loop_blocks = 0;
    }

    /// Zorunlu araçsız döngü özetini işe çevirir; `true` = devam.
    pub fn release_loop(&mut self, final_content: &str, task_goal: &str) -> bool {
        if self.loop_blocks < LOOP_BLOCK_LIMIT {
            return false;
        }
        self.loop_blocks = 0;
        if self.loop_releases >= MAX_LOOP_RELEASES {
            self.loop_exhausted = true;
            return false;
        }
        if !fancy_match(imperative_re(), task_goal) {
            return false;
        }
        if self.write_epoch > 0 && !fancy_match(handoff_re(), tail_chars(final_content, 1500)) {
            return false;
        }
        self.loop_releases += 1;
        self.verify_note = "[EVREN] DÖNGÜ ÖZETİ ALINDI; araçlar yeniden açık. Özetindeki 'sonraki adım'ı ŞİMDİ uygula: edit_file/write_file ile düzenle, sonra tek doğrulama çalıştır. Daha önce yaptığın arama ve okumaları tekrarlama; eksik bilgi varsa FARKLI bir sorgu ya da satır aralığı kullan.".into();
        true
    }

    /// Son yazmadan beri aynı argümanlarla yapılmış çağrıyı reddeder.
    pub fn check(&mut self, fn_name: &str, args: &Value) -> String {
        if repeat_exempt().contains(fn_name) {
            return String::new();
        }
        let signature = call_signature(fn_name, args);
        if self.call_epochs.get(&signature) == Some(&self.write_epoch) {
            self.loop_blocks += 1;
            return "TEKRAR ENGELLENDİ: Bu çağrı bu görevde aynı argümanlarla zaten çalıştı ve o zamandan beri hiçbir dosya değişmedi; sonucu yukarıda. Aynı aracı aynı argümanla tekrar çağırma: bulduğun bilgiyle sıradaki adıma geç (edit_file ile düzenle, testi çalıştır) ya da sonucu bildir.".into();
        }
        // Yalnız yeniden çalışacak çağrılar (arada yazma var) kesiciye ulaşır.
        let broken = self.repeat_breaker(&signature);
        if !broken.is_empty() {
            return broken;
        }
        self.call_epochs.insert(signature, self.write_epoch);
        if fn_name == "run_command" {
            let cmd = arg_str(args, "command").trim_start();
            if arg_bool(args, "use_python")
                || cmd.starts_with("python -c")
                || cmd.starts_with("py -c")
            {
                let probes = self.probe_runs.get(&self.write_epoch).copied().unwrap_or(0) + 1;
                self.probe_runs.insert(self.write_epoch, probes);
                if probes == PROBE_NUDGE_AT {
                    self.probe_note = format!(
                        "[EVREN] {probes} deneme betiği çalıştırdın ve dosya değişmedi. Deneme yeter: bulguna göre kodu şimdi düzenle (edit_file) ve tek doğrulama çalıştır."
                    );
                }
            }
        }
        let family = command_family(fn_name, args);
        if !family.is_empty() {
            let count = match self.family_runs.get(&family) {
                Some((epoch, n)) if *epoch == self.write_epoch => n + 1,
                _ => 1,
            };
            self.family_runs
                .insert(family.clone(), (self.write_epoch, count));
            if count > FAMILY_RUN_LIMIT {
                self.loop_blocks += 1;
                return format!(
                    "TEKRAR ENGELLENDİ: '{family}' bu görevde dosya değişmeden {} kez çalıştı (farklı bayraklarla). Yeni ölçüm yapma: elindeki sonuçlarla bir düzenleme yap ya da bulguyu ve önerini bildir.",
                    count - 1
                );
            }
        }
        String::new()
    }

    /// Yazmaları izler ve bu görevde daha önce görülen çıktıları işaretler.
    pub fn after_tool(&mut self, fn_name: &str, args: &Value, result: &str) -> String {
        self.note_breaker_output(fn_name, args, result);
        if ["edit_file", "write_file", "ssh_write_file"].contains(&fn_name)
            && result.starts_with("BAŞARILI")
        {
            self.write_epoch += 1;
            let written = arg_str(args, "path").replace('\\', "/");
            let written = written.strip_prefix("./").unwrap_or(&written);
            let ext = Path::new(written)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!(".{}", e.to_lowercase()))
                .unwrap_or_default();
            if CODE_EXTS.contains(&ext.as_str()) && !written.starts_with(".evren/") {
                self.code_written = true;
                self.verified_since_write = false;
            }
            return result.to_string();
        }
        let command = arg_str(args, "command");
        if fn_name == "run_command" && test_cmd_re().is_match(command) {
            self.verified_since_write = true;
        }
        if (fn_name == "run_command" || fn_name == "ssh_run") && command_may_modify_files(command) {
            self.write_epoch += 1;
            return result.to_string();
        }
        if repeat_exempt().contains(fn_name) || result.chars().count() < 200 {
            return result.to_string();
        }
        let stripped = output_noise_re().replace_all(result, "");
        let mut hasher = DefaultHasher::new();
        stripped.hash(&mut hasher);
        let key = (fn_name.to_string(), hasher.finish());
        let seen = self.output_seen.get(&key).copied().unwrap_or(0) + 1;
        self.output_seen.insert(key, seen);
        if seen >= 4 && fn_name == "run_command" && self.code_written {
            self.loop_blocks += 1;
            return format!(
                "{result}\n[EVREN] AYNI HATA {seen}. KEZ: yaptığın düzenlemeler bu hatayı değiştirmiyor. Daha fazla deneme yapma: değişikliklerini geri al (git checkout -- <dosya>) ya da son çalışan hale getir, sonra bulguyu 'kısmi' olarak bildir."
            );
        }
        if seen >= 2 {
            if seen >= 3 {
                self.loop_blocks += 1;
            }
            return format!(
                "{result}\n[EVREN] Bu çıktının aynısı bu görevde {seen}. kez geldi; aynı bilgiyi yeniden arama, yaklaşımı değiştir ya da sıradaki adıma geç."
            );
        }
        result.to_string()
    }

    fn idle_note(&mut self, todos: &TodoStore, task_goal: &str) -> String {
        if self.write_epoch != self.idle_epoch {
            self.idle_epoch = self.write_epoch;
            self.idle_steps = 0;
            return String::new();
        }
        self.idle_steps += 1;
        let n = self.idle_steps;
        let open: Vec<_> = todos
            .items()
            .iter()
            .filter(|t| t.status != "completed")
            .collect();
        if open.is_empty() {
            if fancy_match(imperative_re(), task_goal) && (n == 6 || n == 10) {
                return format!(
                    "[EVREN] İLERLEME YOK: {n} adımdır dosya değişmedi. Keşif yeterli; istenen dosyayı/değişikliği şimdi yaz (write_file/edit_file), eksik ayrıntıyı varsayımla doldur."
                );
            }
            if n == 8 || n == 14 {
                return format!(
                    "[EVREN] İLERLEME YOK: {n} adımdır hiçbir dosya değişmedi. Analiz yeterli: bulgunu ve somut önerini şimdi bildir ya da tek bir düzenlemeyi yap; yeni ölçüm başlatma."
                );
            }
            return String::new();
        }
        if !(n == 4 || n == 7 || n == 10) {
            return String::new();
        }
        let nxt = open
            .iter()
            .find(|t| t.status == "in_progress")
            .map(|t| t.content.as_str())
            .unwrap_or(open[0].content.as_str());
        let nxt: String = nxt.chars().take(160).collect();
        format!(
            "[EVREN] İLERLEME YOK: {n} adımdır hiçbir dosya değişmedi. Keşfi ve aynı testi yeniden koşmayı bırak. Şimdi şu maddeyi uygula (edit_file/write_file): {nxt}. Uygulayamıyorsan engeli tek cümleyle bildir."
        )
    }

    /// Bu isteğin kuyruk kullanıcı mesajı için geçici notlar.
    pub fn turn_notes(
        &mut self,
        step_count: u32,
        max_steps: u32,
        stalled_steps: u32,
        listing_done: bool,
        todos: &TodoStore,
        task_goal: &str,
    ) -> String {
        let mut notes: Vec<String> = Vec::new();
        let base = step_notes(step_count, max_steps, stalled_steps);
        if !base.is_empty() {
            notes.push(base);
        }
        if !self.role_report.is_empty()
            && max_steps >= 10
            && max_steps.saturating_sub(step_count) == 5
        {
            notes.push(format!(
                "[EVREN] RAPOR: 5 adım kaldı ({step_count}/{max_steps}). {} henüz yazılmadıysa şimdi yaz; rapor olmadan biten tur boşa gider. Sonra kalan adımlarda yalnız eksikleri tamamla.",
                self.role_report
            ));
        }
        if self.loop_blocks >= LOOP_BLOCK_LIMIT {
            notes.push("[EVREN] DÖNGÜ: aynı araç çağrılarını tekrarlıyorsun. Bu tur araç kullanma; şimdiye kadarki bulgularla yapılanı, kalan işi ve tek somut sonraki adımı yaz.".into());
        }
        if listing_done {
            notes.push("[EVREN] Liste kullanıcının terminalinde tam gösterildi. Araç çağırma; tabloyu yeniden yazmadan 1-3 cümlelik kısa özetle bitir.".into());
        }
        if !self.verify_note.is_empty() {
            notes.push(std::mem::take(&mut self.verify_note));
        }
        let idle = self.idle_note(todos, task_goal);
        if !idle.is_empty() {
            notes.push(idle);
        }
        if !self.probe_note.is_empty() {
            notes.push(std::mem::take(&mut self.probe_note));
        }
        notes.join("\n")
    }
}

/// Adım başına kısa hatırlatmalar; sıradan adımda boş.
pub fn step_notes(step_count: u32, max_steps: u32, stalled_steps: u32) -> String {
    let mut notes: Vec<String> = Vec::new();
    if max_steps >= 8 && max_steps.saturating_sub(step_count) == 3 {
        notes.push(format!(
            "[EVREN] ADIM UYARISI: 3 adım kaldı ({step_count}/{max_steps}). Değişiklikleri toparla ve şimdi doğrulamayı (test/kontrol komutu) çalıştır; yeni keşif başlatma."
        ));
    }
    if max_steps > 1 && max_steps.saturating_sub(step_count) <= 1 {
        notes.push(format!(
            "[EVREN] ADIM UYARISI: adım sınırına geldin ({step_count}/{max_steps}). Yeni araç turu açma; sonucu, kalan işi ve tek sonraki adımı kısa bildir."
        ));
    }
    if stalled_steps > 0 {
        notes.push("[EVREN] Önceki yanıt kesildi veya boş kaldı. Uzun düşünmeyi tekrarlama; gerekli aracı hemen tam argümanlarla çağır veya engeli kısa bildir.".into());
    }
    notes.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn regexes() {
        assert!(command_may_modify_files("git checkout -- a"));
        assert!(command_may_modify_files("echo x > f"));
        assert!(!command_may_modify_files("echo x 2>/dev/null"));
        assert!(!command_may_modify_files("git status"));
        assert!(is_listing_task("dosyaları listele"));
        assert!(!is_listing_task("hatalı dosyaları listele"));
        assert!(fancy_match(imperative_re(), "testi düzelt"));
        assert!(!fancy_match(imperative_re(), "düzeltme yapma"));
        assert!(fancy_match(handoff_re(), "Kalan iş: testler"));
        assert!(!fancy_match(handoff_re(), "Kalan iş: yok"));
        assert!(fancy_match(asks_approval_re(), "Uygulayayım mı?"));
        assert!(test_cmd_re().is_match("cargo test -q"));
        assert_eq!(stall_token_cap(), 16384);
        let calls = vec![
            json!({"function": {"name": "a", "arguments": "{\"x\":1}"}}),
            json!({"function": {"name": "b", "arguments": "{\"x\":"}}),
        ];
        assert!(!tool_arguments_complete(&calls));
        assert_eq!(cut_tool_call_name(&calls), "b");
        assert!(tool_arguments_complete(&calls[..1]));
        assert!(!tool_arguments_complete(&[]));
    }

    #[test]
    fn read_only_commands() {
        assert_eq!(is_read_only_command("  "), (false, "Boş komut.".into()));
        assert_eq!(
            is_read_only_command("echo $(ls)").1,
            "Komut '$(' içeriyor; ask/plan modunda alt kabuk çalıştırılamaz."
        );
        assert_eq!(
            is_read_only_command("git status && ls -la"),
            (true, String::new())
        );
        assert_eq!(
            is_read_only_command("ls > out").1,
            "Komut '>' içeriyor; ask/plan modunda yalnızca basit salt-okuma komutlarına izin verilir."
        );
        assert_eq!(
            is_read_only_command("rm -rf x").1,
            "'rm' komutu salt-okuma listesinde değil."
        );
        assert_eq!(
            is_read_only_command("git -C x push").1,
            "'git push' salt-okuma alt komutu değil."
        );
        assert_eq!(
            is_read_only_command("git branch yeni").1,
            "'git branch <ad>' yeni ref oluşturur; ask/plan modunda yalnız listeleme."
        );
        assert_eq!(is_read_only_command("git branch -a"), (true, String::new()));
        assert_eq!(
            is_read_only_command("git tag -d v1").1,
            "'git tag' bu bayrakla değişiklik yapar; ask/plan modunda yalnız listeleme."
        );
        assert_eq!(
            is_read_only_command("git remote add o u").1,
            "'git remote' yalnız listeleme (-v, show, get-url) ile kullanılabilir."
        );
        assert_eq!(
            is_read_only_command("python app.py").1,
            "ask/plan modunda python yalnızca --version veya -c ile sınırlıdır."
        );
        assert_eq!(
            is_read_only_command("C:\\Tools\\Grep.EXE -n x f"),
            (true, String::new())
        );
        assert_eq!(
            is_read_only_command("grep \"a;b\" f"),
            (true, String::new())
        );
    }

    #[test]
    fn mode_checks() {
        assert!(check_tool_allowed("write_file", &json!({}), Mode::Normal).0);
        assert!(check_tool_allowed("view_file", &json!({}), Mode::Ask).0);
        assert_eq!(
            check_tool_allowed("write_file", &json!({}), Mode::Ask).1,
            "'write_file' ask modunda devre dışı (salt-okuma). Yazmak için normal moda geçin."
        );
        assert_eq!(
            check_tool_allowed("write_file", &json!({}), Mode::Plan).1,
            "'write_file' plan modunda devre dışı (kaynak dosyalara dokunulmaz). Uygulamak için normal moda geçin."
        );
        assert!(check_tool_allowed("create_plan", &json!({}), Mode::Plan).0);
        assert_eq!(
            check_tool_allowed("create_plan", &json!({}), Mode::Ask).1,
            "'create_plan' ask modunda devre dışı. Plan moduna geçin."
        );
        assert_eq!(
            check_tool_allowed(
                "run_command",
                &json!({"command": "ls", "run_in_background": true}),
                Mode::Ask
            )
            .1,
            "ask modunda arka plan komutu başlatılamaz (run_in_background). Normal moda geçin."
        );
        assert_eq!(
            check_tool_allowed("run_command", &json!({"command": "rm x"}), Mode::Plan).1,
            "plan modunda komut reddedildi: 'rm' komutu salt-okuma listesinde değil."
        );
        assert_eq!(
            check_tool_allowed("delegate", &json!({}), Mode::Ask).1,
            "'delegate' yalnızca normal modda kullanılabilir."
        );
    }

    #[test]
    fn families_and_guard() {
        assert_eq!(
            command_family("run_command", &json!({"command": "python -m pytest -q"})),
            "python -m pytest"
        );
        // Python paritesi: `&&` boşluktan sonra eşleşmez, `cd` ile başlayan tek parça atılır.
        assert_eq!(
            command_family(
                "run_command",
                &json!({"command": "cd x && python -m pytest -q"})
            ),
            ""
        );
        assert_eq!(
            command_family("run_command", &json!({"command": "curl -s https://a.b/c"})),
            "curl a.b"
        );
        assert_eq!(
            command_family("run_command", &json!({"command": "dotnet test --no-build"})),
            "dotnet test"
        );
        assert_eq!(
            command_family("run_command", &json!({"command": "python x.py a"})),
            ""
        );
        assert_eq!(command_family("view_file", &json!({})), "");

        let mut g = RepeatGuard::new();
        let args = json!({"path": "a.py"});
        assert_eq!(g.check("view_file", &args), "");
        assert!(g.check("view_file", &args).starts_with("TEKRAR ENGELLENDİ"));
        assert_eq!(g.loop_blocks, 1);
        g.after_tool("edit_file", &json!({"path": "./src/a.py"}), "BAŞARILI");
        assert_eq!(g.write_epoch, 1);
        assert!(g.code_written);
        assert_eq!(g.check("view_file", &args), "");
        for flag in ["-q", "-v", "-s", "-l"] {
            assert_eq!(
                g.check("run_command", &json!({"command": format!("pytest {flag}")})),
                ""
            );
            g.after_tool("run_command", &json!({"command": "pytest"}), "x");
        }
        // Aynı aile, farklı bayrak: 5. çalıştırma engellenir.
        assert!(g
            .check("run_command", &json!({"command": "pytest -x"}))
            .contains("'pytest' bu görevde"));
        let big = "hata ".repeat(50);
        assert_eq!(g.after_tool("find_files", &json!({}), &big), big);
        assert!(g
            .after_tool("find_files", &json!({}), &big)
            .contains("2. kez geldi"));
        assert_eq!(step_notes(5, 8, 0), "[EVREN] ADIM UYARISI: 3 adım kaldı (5/8). Değişiklikleri toparla ve şimdi doğrulamayı (test/kontrol komutu) çalıştır; yeni keşif başlatma.");
        assert!(step_notes(7, 8, 1).contains("adım sınırına geldin (7/8)"));
    }

    #[test]
    fn release_and_idle() {
        let mut g = RepeatGuard::new();
        assert!(!g.release_loop("", "düzelt"));
        g.loop_blocks = 3;
        assert!(g.release_loop("Tek somut sonraki adım: x", "testi düzelt"));
        assert_eq!(g.loop_blocks, 0);
        assert!(g.verify_note.starts_with("[EVREN] DÖNGÜ ÖZETİ ALINDI"));
        let todos = TodoStore::new();
        let notes = g.turn_notes(1, 10, 0, false, &todos, "testi düzelt");
        assert!(notes.starts_with("[EVREN] DÖNGÜ ÖZETİ ALINDI"));
        assert!(g.verify_note.is_empty());
        for _ in 0..4 {
            assert_eq!(g.turn_notes(1, 10, 0, false, &todos, "testi düzelt"), "");
        }
        assert!(g
            .turn_notes(1, 10, 0, false, &todos, "testi düzelt")
            .contains("6 adımdır dosya değişmedi"));
    }
}
