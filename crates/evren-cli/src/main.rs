//! `evren` komut satırı girişi. Argüman yüzeyi Python `cli.build_parser`
//! ile aynıdır.

use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use evren_agent::prompt_frame::{
    build_user_content, collect_prompt_frame, compose_system, delivery_summary, Collected,
    FrameError, PromptFrame, FORM_INTRO,
};
use evren_api::{
    ensure_terms_accepted, ensure_terms_for_keys, mask_key, ApiClient, ChatRequest, StreamDelta,
    TermsOptions, TermsUi,
};
use evren_core::config::{get_workspace_dir, Config};
use evren_core::{models, texts, EvrenError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

mod headless;
mod plan;
mod stdio;
mod terminal_setup;

#[derive(Parser, Debug)]
#[command(
    name = "evren",
    about = "EVREN CLI: AI Development & Project Assistant",
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// Sürümü göster
    #[arg(long, short = 'V')]
    version: bool,
    /// Çalışma dizini (varsayılan: geçerli dizin)
    #[arg(long, short = 'w')]
    workspace: Option<PathBuf>,
    /// Kullanılacak model (varsayılan: EVREN_DEFAULT_MODEL, yoksa mimo-v2.6-pro)
    #[arg(long, short = 'm')]
    model: Option<String>,
    /// Tek istek çalıştır ve çık (headless)
    #[arg(short = 'p', long = "print", value_name = "İSTEK")]
    print_prompt: Option<String>,
    /// Çıktı biçimi
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    output: String,
    /// Ajan için en fazla adım
    #[arg(long)]
    max_steps: Option<u32>,
    /// İzin verilen araçlar (virgülle)
    #[arg(long)]
    allowed_tools: Option<String>,
    /// Tüm onayları otomatik ver
    #[arg(long)]
    yes: bool,
    /// Son oturuma devam et
    #[arg(long = "continue")]
    continue_session: bool,
    /// Rol profili: analyst, developer, tester, shipper ya da .evren/roles/<ad>.json
    #[arg(long)]
    role: Option<String>,
    /// Rol profili dosyası (.json); --role yerine
    #[arg(long)]
    profile: Option<String>,
    /// Ek çalışma kökü AD=YOL (yinelenebilir); araçlarda `AD:yol` ya da root="AD"
    #[arg(long = "root", value_name = "AD=YOL")]
    roots: Vec<String>,
    /// Görev referansı (iz kaydına yazılır; sdlc görev kimliği)
    #[arg(long)]
    task_ref: Option<String>,
    /// NDJSON stdio sunucusu (v1 protokolü)
    #[arg(long)]
    stdio: bool,
    /// Yedekleme modu
    #[arg(long)]
    backup: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Modelle tek seferlik sohbet
    Chat {
        /// Serbest istek. Kalıp alanları da doluysa ek istek olarak eklenir
        prompt: Option<String>,
        #[command(flatten)]
        frame: FrameArgs,
        #[arg(long, short = 's')]
        stream: bool,
        #[arg(long)]
        max_tokens: Option<u32>,
        /// Bağlam olarak eklenecek dosya
        #[arg(long, short = 'f')]
        file: Vec<String>,
    },
    /// Otonom çok adımlı kodlama görevi yürüt
    Agent {
        /// Serbest istek. Kalıp alanları da doluysa ek istek olarak eklenir
        prompt: Option<String>,
        #[command(flatten)]
        frame: FrameArgs,
        /// Maksimum adım sayısı (varsayılan: EVREN_MAX_STEPS, 30)
        #[arg(long = "max-steps")]
        max_steps: Option<u32>,
    },
    /// Projeyi tara
    Scan {
        #[arg(default_value = ".")]
        path: String,
    },
    /// Kota durumunu göster
    Quota,
    /// Kullanım şartları
    Terms {
        #[arg(long)]
        force: bool,
    },
    /// Kullanılabilir modelleri listele
    Models,
    /// CLI güncelle
    Update {
        #[arg(long)]
        no_restart: bool,
    },
    /// Dosyayı yedekten geri al
    Rollback { file: String },
    /// agent.md / changelog.md bağlam dosyalarını yönet
    Docs {
        /// İşlem (varsayılan: status)
        #[arg(default_value = "status", value_parser = ["status", "refresh", "unmark", "path"])]
        docs_action: String,
        /// Hedef proje dizini
        #[arg(long, short = 'w')]
        workspace: Option<PathBuf>,
        /// Yönetilen blokları olmayan bir agent.md dosyasına ekle
        #[arg(long)]
        adopt: bool,
        /// Onay sormadan uygula (yalnızca unmark için)
        #[arg(long)]
        yes: bool,
    },
    /// Hata ayıklama araçları (görev izi)
    Debug {
        #[command(subcommand)]
        action: Option<DebugAction>,
    },
    /// Hedefe tur tur ilerle: kontrol → plan → düzelt → doğrula → HANDOFF
    Iterate {
        /// İterasyon hedefi
        goal: Vec<String>,
        /// En fazla tur (varsayılan 10)
        #[arg(long, default_value_t = 10)]
        rounds: u32,
        /// Kontrol komutu (tekrarlanabilir; varsayılan otomatik)
        #[arg(long)]
        check: Vec<String>,
        /// Madde başına en fazla ajan adımı
        #[arg(long = "item-steps", default_value_t = 40)]
        item_steps: u32,
        /// Son iterasyona kaldığı yerden devam et
        #[arg(long)]
        resume: bool,
        /// Salt-okunur referans klasör (ör. web projesi)
        #[arg(long = "read-root")]
        read_root: Vec<PathBuf>,
        /// Son iterasyonun durumunu göster
        #[arg(long)]
        status: bool,
        #[arg(long, short = 'm')]
        model: Option<String>,
        #[arg(long, short = 'w')]
        workspace: Option<PathBuf>,
    },
    /// Planlayıcının JSON adım listesini LLM'siz, refleks hızında çalıştır
    Plan {
        plan_file: String,
        #[arg(long)]
        dry_run: bool,
        /// push adımlarına izin ver (kullanıcı onayı)
        #[arg(long)]
        allow_push: bool,
        /// N. adımdan devam et
        #[arg(long = "from", default_value_t = 1)]
        start: usize,
        #[arg(long, short = 'w')]
        workspace: Option<PathBuf>,
    },
    /// Windows Terminal'e 'EVREN (Git Bash)' profili ekler
    #[command(name = "setup-terminal", visible_alias = "terminal")]
    Terminal {
        /// Başlangıç klasörü (varsayılan: kullanıcı klasörü)
        #[arg(long)]
        dir: Option<String>,
        /// Masaüstüne kısayol da oluştur
        #[arg(long)]
        shortcut: bool,
        /// Profili ve kısayolu kaldır
        #[arg(long)]
        remove: bool,
    },
    /// SSH yönetimi (add/rm/test/run)
    Ssh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum DebugAction {
    /// Son ajan görevinin adım tablosunu göster (istek, araç, süre, token)
    Last {
        /// Sondan N. görevi göster (varsayılan: 1 = son görev)
        #[arg(
            long,
            short = 'n',
            default_value_t = 1,
            value_name = "N",
            allow_negative_numbers = true
        )]
        task: i64,
    },
}

/// Beş satırlık kalıp bayrakları (Python `_add_frame_args`). Rol/Çerçeve
/// system'e, kalanı user'a gider.
#[derive(Args, Debug, Default, Clone)]
struct FrameArgs {
    /// Kimlik: modelin hangi sesle yazacağı (system)
    #[arg(long)]
    rol: Option<String>,
    /// İş çerçevesi; strateji yalnızca bu alana yüklenir (system)
    #[arg(long)]
    cerceve: Option<String>,
    /// Görev, tek cümle (user)
    #[arg(long)]
    brief: Option<String>,
    /// Ölçek, tipografi, design system (user)
    #[arg(long)]
    kisit: Option<String>,
    /// Format ve teslim yüzeyi (user)
    #[arg(long)]
    cikti: Option<String>,
}

impl FrameArgs {
    fn frame(&self) -> PromptFrame {
        let f = |v: &Option<String>| v.clone().unwrap_or_default();
        PromptFrame::new(
            &f(&self.rol),
            &f(&self.cerceve),
            &f(&self.brief),
            &f(&self.kisit),
            &f(&self.cikti),
        )
    }
}

const FRAME_REQUIRED: &str =
    "Bir istek veya en az bir kalıp alanı gerekli (--rol, --cerceve, --brief, --kisit, --cikti).";

/// Git Bash "/ask ..." argümanını "C:/Program Files/Git/ask ..." yapar;
/// yalnız çıplak bir kelimeden önceki Git kurulum öneki geri alınır
/// (Python `_undo_msys_path`).
fn undo_msys_path(text: &str) -> String {
    let b = text.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || &b[1..3] != b":/" {
        return text.to_owned();
    }
    let rest = &text[3..];
    let rest = rest.strip_prefix("Program Files/").unwrap_or(rest);
    let Some(word) = rest.strip_prefix("Git/") else {
        return text.to_owned();
    };
    let end = word
        .find(|c: char| !(c.is_ascii_alphabetic() || c == '?' || c == '-'))
        .unwrap_or(word.len());
    let ok = end > 0 && word[end..].chars().next().is_none_or(char::is_whitespace);
    if ok {
        format!("/{word}")
    } else {
        text.to_owned()
    }
}

/// Kalıp ve serbest isteği çözer (Python `load_frame`). Bayraklar önce
/// gelir; ikisi de yoksa uçbirimde beş satır sorulur. Gönderilecek bir şey
/// yoksa `None`.
fn load_frame(
    args: &FrameArgs,
    prompt: Option<&str>,
    tty: bool,
    read_line: &mut dyn FnMut(&str) -> Result<String, FrameError>,
) -> Result<Option<(PromptFrame, String)>, FrameError> {
    let mut frame = args.frame();
    let mut prompt = undo_msys_path(prompt.unwrap_or_default()).trim().to_owned();
    if frame.is_empty() && prompt.is_empty() && tty {
        print_info(FORM_INTRO);
        match collect_prompt_frame(read_line, &evren_tui::commands::is_command_input, None)? {
            Collected::Command(_) => {
                print_warning("Slash komutları yalnızca interaktif REPL içindedir.");
                return Ok(None);
            }
            Collected::Frame(f) => frame = f,
        }
        if frame.is_empty() {
            prompt = read_line("İstek > ")?.trim().to_owned();
        }
    }
    if frame.is_empty() && prompt.is_empty() {
        return Ok(None);
    }
    Ok(Some((frame, prompt)))
}

/// `input()` karşılığı: etiketi basar, bir satır okur.
fn stdin_line(label: &str) -> Result<String, FrameError> {
    print!("{label}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => Err(FrameError::Eof),
        Ok(_) => Ok(line.trim_end_matches(['\r', '\n']).to_owned()),
        Err(e) => Err(FrameError::Failed(e.to_string())),
    }
}

/// Bayrak/form ile kalıbı yükler; boşsa hatayı basar.
fn resolve_frame(args: &FrameArgs, prompt: Option<&str>) -> Option<(PromptFrame, String)> {
    let tty = std::io::stdin().is_terminal();
    match load_frame(args, prompt, tty, &mut stdin_line) {
        Ok(Some(pair)) => Some(pair),
        Ok(None) => {
            print_error(FRAME_REQUIRED);
            None
        }
        Err(FrameError::Failed(e)) => {
            print_error(&e);
            None
        }
        Err(_) => None,
    }
}

fn print_info(msg: &str) {
    println!("[*] {msg}");
}

fn print_success(msg: &str) {
    println!("[+] {msg}");
}

fn print_warning(msg: &str) {
    println!("[!] {msg}");
}

fn print_error(msg: &str) {
    eprintln!("[X] {msg}");
}

fn status(msg: &str) {
    eprintln!("{msg}");
}

/// Durum satırını (stderr) siler; ANSI destekli uçbirimler için.
fn clear_status() {
    eprint!("\r\x1b[2K");
}

/// Binlik ayraçlı sayı (`f"{n:,}"`).
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut groups: Vec<&str> = Vec::new();
    let mut end = digits.len();
    while end > 3 {
        groups.push(&digits[end - 3..end]);
        end -= 3;
    }
    groups.push(&digits[..end]);
    groups.reverse();
    let joined = groups.join(",");
    if n < 0 {
        format!("-{joined}")
    } else {
        joined
    }
}

/// `--allowed-tools a,b` → `EXTRA_ALLOW` kural listesi.
fn split_tools(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn headless() -> bool {
    std::env::var("EVREN_HEADLESS")
        .map(|v| v == "1")
        .unwrap_or(false)
        || !std::io::stdin().is_terminal()
}

/// `ui.prompt_confirm`: başsız kipte varsayılanı yazıp döner.
fn prompt_confirm(msg: &str, default_yes: bool) -> bool {
    if headless() {
        println!("{}", texts::confirm_headless(msg, default_yes));
        return default_yes;
    }
    print!("{}", texts::confirm_prompt(msg, default_yes));
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!("{}", texts::confirm_no_input(default_yes));
            default_yes
        }
        Ok(_) => {
            let a = line.trim().to_lowercase();
            if a.is_empty() {
                default_yes
            } else {
                matches!(a.as_str(), "y" | "yes" | "evet" | "e")
            }
        }
    }
}

/// Şartlar akışının CLI çıktısı.
struct CliTermsUi;

impl TermsUi for CliTermsUi {
    fn info(&mut self, msg: &str) {
        print_info(msg);
    }
    fn success(&mut self, msg: &str) {
        print_success(msg);
    }
    fn warning(&mut self, msg: &str) {
        print_warning(msg);
    }
    fn error(&mut self, msg: &str) {
        print_error(msg);
    }
    fn raw(&mut self, msg: &str) {
        println!("{msg}");
    }
    fn confirm(&mut self, msg: &str) -> bool {
        prompt_confirm(msg, false)
    }
}

/// `evren update`: en son sürümü çeker, kurar ve (isteğe bağlı) yeniden başlatır.
fn cmd_update(no_restart: bool) -> ExitCode {
    use evren_tui::updater;
    print_info(&updater::source_line());
    if let Some(warn) = updater::dirty_tree_warning() {
        evren_tui::ui::print_warning(&warn);
    }
    print_info(updater::SPINNER_TEXT);
    let (ok, msg) = updater::perform_update();
    if !ok {
        print_error(&format!("Güncelleme başarısız: {msg}"));
        return ExitCode::from(1);
    }
    if updater::update_is_deferred() {
        print_success(&msg);
    } else {
        print_success("Güncelleme tamamlandı.");
        if !no_restart {
            print_info("CLI yeni sürümle yeniden başlatılıyor...");
            let err = updater::restart_process();
            print_error(&err);
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

/// `evren debug [last -n N]`: son ajan görevinin adım tablosu (Python `cmd_debug`).
fn cmd_debug(config: &Config, workspace: &std::path::Path, task: i64) -> ExitCode {
    use evren_agent::trace_report::{last_task_report, DebugReport};
    let index = usize::try_from(task).unwrap_or(0);
    match last_task_report(workspace, index, config.trace_enabled()) {
        DebugReport::Warning(m) => print_warning(&m),
        DebugReport::Info(m) => print_info(&m),
        DebugReport::Lines(lines) => {
            for line in lines {
                println!("{line}");
            }
        }
    }
    ExitCode::SUCCESS
}

/// `evren scan [yol]`: dosya ağacı ve uzantı dağılımı (Python `cmd_scan`).
fn cmd_scan(path: &str) -> ExitCode {
    use evren_core::project_scan::{generate_tree, scan_workspace};
    let target = evren_core::tools::file_ops::resolve_path(std::path::Path::new(path));
    status(&format!(
        "'{}' taranıyor ve dosya ağacı oluşturuluyor...",
        target.display()
    ));
    let tree = generate_tree(&target, 3);
    let stats = scan_workspace(&target, 150, 250.0);
    println!("{tree}");
    print_info(&format!(
        "Toplam dosya: {} | Boyut: {} KB",
        stats.total_files,
        evren_tui::extras::py_float(stats.total_size_kb)
    ));
    println!("Uzantı Dağılımı:");
    for (ext, count) in &stats.extension_counts {
        println!("  • {ext:<10} : {count} dosya");
    }
    ExitCode::SUCCESS
}

/// `evren quota`: havuzdaki her anahtarın kotası (Python `cmd_quota`).
async fn cmd_quota(config: &Config) -> Result<ExitCode, EvrenError> {
    let api = client(config)?;
    status("EVREN API kota bilgileri alınıyor...");
    let rows = evren_tui::handlers::key_quota_rows(&api).await;
    print_info(&format!("API Kota ({} anahtar):", rows.len()));
    for line in evren_tui::handlers::key_quota_lines(&rows) {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `evren rollback <dosya>`: dosyayı en son yedeğinden geri yükler.
fn cmd_rollback(workspace: &std::path::Path, file: &str) -> ExitCode {
    use evren_core::tools::file_ops::{resolve_workspace_path, rollback_last_backup};
    let target = resolve_workspace_path(file, workspace);
    let (ok, msg) = rollback_last_backup(&target, workspace);
    if ok {
        print_success(&msg);
    } else {
        print_error(&msg);
    }
    ExitCode::SUCCESS
}

/// `evren setup-terminal`: Windows Terminal profili ekler ya da kaldırır.
fn cmd_setup_terminal(
    config: &Config,
    dir: Option<&str>,
    shortcut: bool,
    remove: bool,
) -> ExitCode {
    let lines = if remove {
        terminal_setup::remove()
    } else {
        terminal_setup::setup(config, dir, shortcut)
    };
    for line in lines {
        if line.starts_with("HATA") {
            print_error(&line);
        } else {
            print_info(&line);
        }
    }
    ExitCode::SUCCESS
}

fn client(config: &Config) -> Result<ApiClient, EvrenError> {
    config.require_api_key()?;
    ApiClient::new(config)
}

async fn cmd_models(config: &Config) -> Result<(), EvrenError> {
    let api = client(config)?;
    status(texts::MODELS_FETCHING);
    let list = api.list_models().await;
    print_info(texts::MODELS_HEADER);
    for m in list {
        let extra = models::recommendation(&m)
            .map(|r| format!("  ({r})"))
            .unwrap_or_default();
        println!("  • {m:<24}{extra}");
    }
    Ok(())
}

/// `_pin_best_key`: ≥2 anahtar varsa kotası en yüksek olanı sabitler.
async fn pin_best_key(api: &ApiClient) {
    if api.with_pool(|p| p.len()) < 2 {
        return;
    }
    match api.select_best_key(None).await {
        Ok(best) => print_info(&texts::active_key(
            &mask_key(&best.key),
            &thousands(best.remaining),
        )),
        Err(e) => print_warning(&texts::key_quota_unreadable(&e)),
    }
}

async fn cmd_terms(config: &Config, force: bool) -> Result<ExitCode, EvrenError> {
    let api = client(config)?;
    let keys = config.api_keys();
    let mut ui = CliTermsUi;
    let ok = ensure_terms_for_keys(&api, &keys, !headless(), force, &mut ui).await;
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Akışlı yanıtın uçbirim çıktısı: durum satırı ilk parçada silinir,
/// düşünme başlığı bir kez basılır, içerik ham akar.
#[derive(Default)]
struct StreamPrinter {
    started: bool,
    reasoning_shown: bool,
    content_shown: bool,
}

impl StreamPrinter {
    fn on_delta(&mut self, d: StreamDelta) {
        if !self.started {
            clear_status();
            self.started = true;
        }
        match d {
            StreamDelta::Reasoning(r) => {
                if !self.reasoning_shown {
                    println!("{}", texts::REASONING_HEADER);
                    self.reasoning_shown = true;
                }
                print!("{r}");
            }
            StreamDelta::Content(c) => {
                if self.reasoning_shown && !self.content_shown {
                    println!();
                }
                self.content_shown = true;
                print!("{c}");
            }
            StreamDelta::ToolCalls(_) => {}
        }
        let _ = std::io::stdout().flush();
    }
}

/// `chat --file`: dosya bağlamını isteğin önüne ekler (Python `cmd_chat`).
fn with_file_context(user: String, files: &[String], workspace: &std::path::Path) -> String {
    if files.is_empty() {
        return user;
    }
    let context = evren_core::project_scan::build_context_from_files(files, workspace);
    format!("Aşağıdaki dosya bağlamı verilmiştir:\n\n{context}\n\nİstek:\n{user}")
}

#[allow(clippy::too_many_arguments)]
async fn cmd_chat(
    config: &Config,
    model: Option<String>,
    prompt: Option<String>,
    frame_args: &FrameArgs,
    max_tokens: Option<u32>,
    stream: bool,
    files: &[String],
    workspace: &std::path::Path,
) -> Result<ExitCode, EvrenError> {
    let api = client(config)?;
    // Python'dan sapma: şartlar, havuzdaki en iyi anahtar sabitlendikten
    // sonra o anahtarla kontrol edilir (ilk anahtar geçersizse de çalışır).
    pin_best_key(&api).await;
    let mut ui = CliTermsUi;
    let terms_ok = ensure_terms_accepted(
        &api,
        None,
        TermsOptions {
            interactive: !headless(),
            force: false,
            silent_ok: true,
        },
        &mut ui,
    )
    .await;
    if !terms_ok {
        return Ok(ExitCode::from(1));
    }
    let Some((frame, prompt)) = resolve_frame(frame_args, prompt.as_deref()) else {
        return Ok(ExitCode::from(1));
    };
    let (system, user) = if frame.is_empty() {
        (texts::CHAT_SYSTEM.to_owned(), prompt)
    } else {
        print_info(&format!(
            "Kalıp API'ye ayrıldı: {}",
            delivery_summary(&frame)
        ));
        (
            compose_system(texts::CHAT_SYSTEM, &frame),
            build_user_content(&frame, &prompt),
        )
    };
    let user = with_file_context(user, files, workspace);
    let model = model.unwrap_or_else(|| config.default_model());
    let mut req = ChatRequest::simple(&system, &user);
    req.model = Some(model.clone());
    req.max_tokens = Some(max_tokens.unwrap_or(config.default_max_tokens()));

    if stream {
        status(&texts::model_thinking_stream(&model));
        let mut printer = StreamPrinter::default();
        match api.chat_complete_stream(req, |d| printer.on_delta(d)).await {
            Ok(_) => {
                println!("\n");
                Ok(ExitCode::SUCCESS)
            }
            Err(e) => {
                if !printer.started {
                    clear_status();
                }
                print_error(&texts::api_error(e.kind_name(), &e));
                println!("\n");
                Ok(ExitCode::from(1))
            }
        }
    } else {
        status(&texts::model_thinking(&model));
        let resp = api.chat_complete(req).await?;
        if let Some(reasoning) = resp.reasoning() {
            println!("{}", texts::REASONING_HEADER);
            println!("{reasoning}");
            println!();
        }
        println!("{}", resp.content().unwrap_or(""));
        Ok(ExitCode::SUCCESS)
    }
}

/// `evren agent`: tek seferlik otonom görev (Python `cmd_agent`).
async fn cmd_agent(
    config: &Config,
    workspace: &std::path::Path,
    model: Option<String>,
    prompt: Option<String>,
    frame_args: &FrameArgs,
    max_steps: Option<u32>,
) -> Result<ExitCode, EvrenError> {
    let api = client(config)?;
    // cmd_chat ile aynı sapma: şartlar sabitlenen en iyi anahtarla sorulur.
    pin_best_key(&api).await;
    let mut ui = CliTermsUi;
    let terms_ok = ensure_terms_accepted(
        &api,
        None,
        TermsOptions {
            interactive: !headless(),
            force: false,
            silent_ok: true,
        },
        &mut ui,
    )
    .await;
    if !terms_ok {
        return Ok(ExitCode::from(1));
    }
    let Some((frame, prompt)) = resolve_frame(frame_args, prompt.as_deref()) else {
        return Ok(ExitCode::from(1));
    };
    let task = evren_tui::AgentOnce {
        model,
        frame,
        prompt,
        max_steps,
    };
    Ok(evren_tui::run_agent_once(std::sync::Arc::new(api), config, workspace, task).await)
}

/// Python `cmd_iterate` argümanları.
struct IterateArgs {
    goal: Vec<String>,
    rounds: u32,
    check: Vec<String>,
    item_steps: u32,
    resume: bool,
    read_root: Vec<PathBuf>,
    status: bool,
    model: Option<String>,
}

/// `evren iterate`: kontrol → plan → düzelt → doğrula → HANDOFF, tur tur.
async fn cmd_iterate(
    config: &Config,
    workspace: &std::path::Path,
    args: IterateArgs,
) -> Result<ExitCode, EvrenError> {
    if !args.read_root.is_empty() {
        let mut roots: Vec<PathBuf> = std::env::var_os("EVREN_READ_ROOTS")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        roots.retain(|r| !r.as_os_str().is_empty());
        roots.extend(
            args.read_root
                .iter()
                .map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.clone())),
        );
        if let Ok(joined) = std::env::join_paths(roots) {
            std::env::set_var("EVREN_READ_ROOTS", joined);
        }
    }
    if args.status {
        println!("{}", evren_tui::iteration::status_text(workspace));
        return Ok(ExitCode::SUCCESS);
    }
    let api = client(config)?;
    let mut ui = CliTermsUi;
    let terms_ok = ensure_terms_accepted(
        &api,
        None,
        TermsOptions {
            interactive: true,
            force: false,
            silent_ok: true,
        },
        &mut ui,
    )
    .await;
    if !terms_ok {
        return Ok(ExitCode::SUCCESS);
    }
    pin_best_key(&api).await;
    let task = evren_tui::iteration::IterateCli {
        model: args.model,
        goal: args.goal.join(" "),
        opts: evren_tui::iteration::RunnerOptions {
            checks: args.check,
            max_rounds: args.rounds,
            item_steps: args.item_steps,
            resume: args.resume,
        },
    };
    Ok(evren_tui::iteration::run_cli(std::sync::Arc::new(api), config, workspace, task).await)
}

async fn run(cli: Cli) -> Result<ExitCode, EvrenError> {
    if cli.version {
        println!("evren-cli {}", evren_core::VERSION);
        return Ok(ExitCode::SUCCESS);
    }
    let workspace = get_workspace_dir(cli.workspace.as_deref());
    if cli.workspace.is_some() {
        std::env::set_current_dir(&workspace).map_err(|e| EvrenError::io(&workspace, e))?;
    }
    let headless_run = cli.stdio || (cli.print_prompt.is_some() && cli.command.is_none());
    if headless_run {
        // stdout protokole/sonuca ayrılır; onay/sorular başsız kipte sorulmaz.
        std::env::set_var("EVREN_HEADLESS", "1");
    }
    let config = Config::load();

    if cli.stdio {
        let opts = stdio::ServeOptions {
            model: cli.model.clone(),
            yes: cli.yes,
            allowed_tools: split_tools(cli.allowed_tools.as_deref()),
            continue_session: cli.continue_session,
            role: cli.profile.clone().or_else(|| cli.role.clone()),
            roots: cli.roots.clone(),
            task_ref: cli.task_ref.clone().unwrap_or_default(),
        };
        return stdio::serve(&config, opts, &workspace).await;
    }
    if let (Some(prompt), None) = (cli.print_prompt.clone(), cli.command.as_ref()) {
        let opts = headless::HeadlessOptions {
            prompt,
            model: cli.model.clone(),
            json: cli.output == "json",
            max_steps: cli.max_steps,
            yes: cli.yes,
            allowed_tools: split_tools(cli.allowed_tools.as_deref()),
            continue_session: cli.continue_session,
            role: cli.profile.clone().or_else(|| cli.role.clone()),
            roots: cli.roots.clone(),
            task_ref: cli.task_ref.clone().unwrap_or_default(),
        };
        return headless::run(&config, opts, &workspace).await;
    }
    match cli.command {
        Some(Command::Models) => cmd_models(&config).await.map(|()| ExitCode::SUCCESS),
        Some(Command::Chat {
            prompt,
            frame,
            stream,
            max_tokens,
            file,
        }) => {
            cmd_chat(
                &config, cli.model, prompt, &frame, max_tokens, stream, &file, &workspace,
            )
            .await
        }
        Some(Command::Debug { action }) => {
            let task = match action {
                Some(DebugAction::Last { task }) => task,
                None => 1,
            };
            Ok(cmd_debug(&config, &workspace, task))
        }
        Some(Command::Scan { path }) => Ok(cmd_scan(&path)),
        Some(Command::Quota) => cmd_quota(&config).await,
        Some(Command::Rollback { file }) => Ok(cmd_rollback(&workspace, &file)),
        Some(Command::Terminal {
            dir,
            shortcut,
            remove,
        }) => Ok(cmd_setup_terminal(
            &config,
            dir.as_deref(),
            shortcut,
            remove,
        )),
        Some(Command::Agent {
            prompt,
            frame,
            max_steps,
        }) => cmd_agent(&config, &workspace, cli.model, prompt, &frame, max_steps).await,
        Some(Command::Terms { force }) => cmd_terms(&config, force).await,
        Some(Command::Iterate {
            goal,
            rounds,
            check,
            item_steps,
            resume,
            read_root,
            status,
            model,
            workspace: target,
        }) => {
            let root = match target {
                Some(t) => get_workspace_dir(Some(&t)),
                None => workspace.clone(),
            };
            let args = IterateArgs {
                goal,
                rounds,
                check,
                item_steps,
                resume,
                read_root,
                status,
                model: model.or(cli.model),
            };
            cmd_iterate(&config, &root, args).await
        }
        Some(Command::Docs {
            docs_action,
            workspace: target,
            adopt,
            yes,
        }) => {
            let root = match target {
                Some(t) => get_workspace_dir(Some(&t)),
                None => workspace.clone(),
            };
            Ok(evren_tui::docs::cli_command(
                &config,
                &root,
                &docs_action,
                adopt,
                yes,
            ))
        }
        Some(Command::Plan {
            plan_file,
            dry_run,
            allow_push,
            start,
            workspace: target,
        }) => {
            let root = match target {
                Some(t) => get_workspace_dir(Some(&t)),
                None => workspace.clone(),
            };
            // reqwest::blocking tokio iş parçacığında çalışamaz.
            let res = tokio::task::spawn_blocking(move || {
                plan::run_plan(&plan_file, &root, dry_run, allow_push, start)
            })
            .await;
            Ok(match res {
                Ok(Ok(code)) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
                Ok(Err(e)) => {
                    print_error(&e);
                    ExitCode::from(1)
                }
                Err(e) => {
                    print_error(&e.to_string());
                    ExitCode::from(1)
                }
            })
        }
        Some(Command::Ssh { args }) => Ok(evren_tui::ssh_cmds::cli(&workspace, &config, &args)),
        Some(Command::Update { no_restart }) => Ok(cmd_update(no_restart)),
        None => {
            config.require_api_key()?;
            let opts = evren_tui::ReplOptions {
                model: cli.model,
                continue_last: cli.continue_session,
            };
            evren_tui::run(&config, &workspace, opts).await
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            print_error(&e.to_string());
            ExitCode::from(1)
        }
    }
}

/// stdin satırlarını okuyup stdout'a NDJSON yazar (stdio.rs ile paylaşılır).
pub(crate) async fn write_line(out: &mut tokio::io::Stdout, line: &str) -> std::io::Result<()> {
    out.write_all(line.as_bytes()).await?;
    out.write_all(b"\n").await?;
    out.flush().await
}

pub(crate) fn stdin_lines() -> tokio::io::Lines<BufReader<tokio::io::Stdin>> {
    BufReader::new(tokio::io::stdin()).lines()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn debug_last_and_setup_terminal_parse_like_python() {
        let cli = Cli::parse_from(["evren", "debug"]);
        assert!(matches!(cli.command, Some(Command::Debug { action: None })));
        let cli = Cli::parse_from(["evren", "debug", "last", "-n", "3"]);
        assert!(matches!(
            cli.command,
            Some(Command::Debug {
                action: Some(DebugAction::Last { task: 3 })
            })
        ));
        for name in ["setup-terminal", "terminal"] {
            let cli = Cli::parse_from(["evren", name, "--dir", "x", "--shortcut"]);
            let Some(Command::Terminal {
                dir,
                shortcut,
                remove,
            }) = cli.command
            else {
                unreachable!("setup-terminal ayrıştırılmadı");
            };
            assert_eq!(dir.as_deref(), Some("x"));
            assert!(shortcut && !remove);
        }
    }

    #[test]
    fn chat_file_context_wraps_the_request() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "print(1)").unwrap();
        assert_eq!(with_file_context("soru".into(), &[], dir.path()), "soru");
        let out = with_file_context("soru".into(), &["a.py".to_string()], dir.path());
        assert_eq!(
            out,
            "Aşağıdaki dosya bağlamı verilmiştir:\n\n--- DOSYA: a.py ---\n```py\nprint(1)\n```\n\nİstek:\nsoru"
        );
    }

    fn no_input(_: &str) -> Result<String, FrameError> {
        Err(FrameError::Failed("okunmamalı".into()))
    }

    #[test]
    fn load_frame_from_flags_does_not_need_a_positional() {
        let args = FrameArgs {
            rol: Some("kimlik".into()),
            brief: Some("iş".into()),
            ..FrameArgs::default()
        };
        let loaded = load_frame(&args, None, true, &mut no_input);
        let Ok(Some((frame, prompt))) = loaded else {
            unreachable!("kalıp yüklenmedi");
        };
        assert_eq!(frame.rol, "kimlik");
        assert_eq!(frame.brief, "iş");
        assert_eq!(prompt, "");
    }

    #[test]
    fn empty_input_without_tty_is_nothing_to_send() {
        let loaded = load_frame(&FrameArgs::default(), Some("  "), false, &mut no_input);
        assert!(matches!(loaded, Ok(None)));
    }

    #[test]
    fn form_slash_command_is_refused_outside_repl() {
        let mut read = |_: &str| Ok::<_, FrameError>("/help".to_string());
        let loaded = load_frame(&FrameArgs::default(), None, true, &mut read);
        assert!(matches!(loaded, Ok(None)));
    }

    #[test]
    fn blank_form_falls_back_to_a_single_request() {
        let mut read = |label: &str| {
            Ok::<_, FrameError>(if label == "İstek > " {
                "dosyayı düzelt".to_string()
            } else {
                String::new()
            })
        };
        let loaded = load_frame(&FrameArgs::default(), None, true, &mut read);
        let Ok(Some((frame, prompt))) = loaded else {
            unreachable!("istek okunmadı");
        };
        assert!(frame.is_empty());
        assert_eq!(prompt, "dosyayı düzelt");
    }

    #[test]
    fn msys_prefix_before_a_bare_word_is_undone() {
        assert_eq!(
            undo_msys_path("C:/Program Files/Git/ask neden?"),
            "/ask neden?"
        );
        assert_eq!(undo_msys_path("C:/Git/plan"), "/plan");
        assert_eq!(
            undo_msys_path("C:/Program Files/Git/usr/bin x"),
            "C:/Program Files/Git/usr/bin x"
        );
        assert_eq!(undo_msys_path("C:/Users/a b"), "C:/Users/a b");
        assert_eq!(undo_msys_path("düzelt"), "düzelt");
    }

    #[test]
    fn frame_flags_parse_on_chat_and_agent() {
        let cli = Cli::parse_from([
            "evren",
            "agent",
            "--rol",
            "r",
            "--brief",
            "b",
            "--max-steps",
            "3",
        ]);
        let Some(Command::Agent {
            prompt,
            frame,
            max_steps,
        }) = cli.command
        else {
            unreachable!("agent ayrıştırılmadı");
        };
        assert_eq!(prompt, None);
        assert_eq!(frame.frame().brief, "b");
        assert_eq!(max_steps, Some(3));
        let cli = Cli::parse_from(["evren", "chat", "--cikti", "tablo", "ek"]);
        let Some(Command::Chat { prompt, frame, .. }) = cli.command else {
            unreachable!("chat ayrıştırılmadı");
        };
        assert_eq!(prompt.as_deref(), Some("ek"));
        assert_eq!(frame.frame().cikti, "tablo");
    }
}
