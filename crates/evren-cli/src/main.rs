//! `evren` komut satırı girişi. Argüman yüzeyi Python `cli.build_parser`
//! ile aynıdır; henüz taşınmayan alt komutlar açık bir hata ile çıkar.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use evren_api::{ApiClient, ChatRequest};
use evren_core::config::{get_workspace_dir, Config};
use evren_core::{models, texts, EvrenError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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

fn print_error(msg: &str) {
    eprintln!("[X] {msg}");
}

fn status(msg: &str) {
    eprintln!("{msg}");
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

async fn cmd_chat(
    config: &Config,
    model: Option<String>,
    prompt: Option<String>,
    max_tokens: Option<u32>,
) -> Result<(), EvrenError> {
    let prompt = prompt.unwrap_or_default();
    if prompt.trim().is_empty() {
        return Err(EvrenError::Other("İstek boş olamaz.".to_owned()));
    }
    let api = client(config)?;
    let model = model.unwrap_or_else(|| config.default_model());
    let mut req = ChatRequest::simple(texts::CHAT_SYSTEM, &prompt);
    req.model = Some(model.clone());
    req.max_tokens = Some(max_tokens.unwrap_or(config.default_max_tokens()));
    status(&texts::model_thinking(&model));
    let resp = api.chat_complete(req).await?;
    if let Some(reasoning) = resp.reasoning() {
        println!("💭 Düşünme / Akıl Yürütme");
        println!("{reasoning}");
        println!();
    }
    println!("{}", resp.content().unwrap_or(""));
    Ok(())
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
    let config = Config::load();

    if cli.stdio {
        let model = cli.model.clone().unwrap_or_else(|| config.default_model());
        return stdio::serve(&config, &model, &workspace)
            .await
            .map(|()| ExitCode::SUCCESS);
    }
    if cli.print_prompt.is_some() {
        return Ok(not_ported());
    }
    match cli.command {
        Some(Command::Models) => cmd_models(&config).await.map(|()| ExitCode::SUCCESS),
        Some(Command::Chat {
            prompt,
            stream,
            max_tokens,
            file,
        }) => {
            if stream || !file.is_empty() {
                return Ok(not_ported());
            }
            cmd_chat(&config, cli.model, prompt, max_tokens)
                .await
                .map(|()| ExitCode::SUCCESS)
        }
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
