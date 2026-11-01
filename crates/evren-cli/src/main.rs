//! `evren` komut satırı girişi. Argüman yüzeyi Python `cli.build_parser`
//! ile aynıdır; henüz taşınmayan alt komutlar açık bir hata ile çıkar.

use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use evren_api::{
    ensure_terms_accepted, ensure_terms_for_keys, mask_key, ApiClient, ChatRequest, StreamDelta,
    TermsOptions, TermsUi,
};
use evren_core::config::{get_workspace_dir, Config};
use evren_core::{models, texts, EvrenError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

mod headless;
mod stdio;

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
    /// Kullanılacak model (varsayılan: EVREN_DEFAULT_MODEL)
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
        prompt: Option<String>,
        #[arg(long, short = 's')]
        stream: bool,
        #[arg(long)]
        max_tokens: Option<u32>,
        #[arg(long, short = 'f')]
        file: Vec<PathBuf>,
    },
    /// Ajan modunu başlat
    Agent,
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
    /// Dokümantasyon
    Docs,
    /// Hata ayıklama
    Debug,
    /// Yinelemeli geliştirme
    Iterate,
    /// Plan dosyasını çalıştır
    Plan { plan_file: String },
    /// Terminal modu
    Terminal,
    /// SSH yönetimi (add/rm/test/run)
    Ssh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
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

fn not_ported() -> ExitCode {
    print_error(texts::NOT_PORTED_YET);
    ExitCode::from(1)
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

async fn cmd_chat(
    config: &Config,
    model: Option<String>,
    prompt: Option<String>,
    max_tokens: Option<u32>,
    stream: bool,
) -> Result<ExitCode, EvrenError> {
    let prompt = prompt.unwrap_or_default();
    if prompt.trim().is_empty() {
        return Err(EvrenError::Other("İstek boş olamaz.".to_owned()));
    }
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
    let model = model.unwrap_or_else(|| config.default_model());
    let mut req = ChatRequest::simple(texts::CHAT_SYSTEM, &prompt);
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
        };
        return stdio::serve(&config, opts, &workspace)
            .await
            .map(|()| ExitCode::SUCCESS);
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
        };
        return headless::run(&config, opts, &workspace).await;
    }
    match cli.command {
        Some(Command::Models) => cmd_models(&config).await.map(|()| ExitCode::SUCCESS),
        Some(Command::Chat {
            prompt,
            stream,
            max_tokens,
            file,
        }) => {
            if !file.is_empty() {
                return Ok(not_ported());
            }
            cmd_chat(&config, cli.model, prompt, max_tokens, stream).await
        }
        Some(Command::Terms { force }) => cmd_terms(&config, force).await,
        Some(_) | None => Ok(not_ported()),
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
