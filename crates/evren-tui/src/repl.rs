//! Etkileşimli REPL (Python `repl.py` `start_repl` / `_repl_loop`).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use evren_agent::modes::mode_label;
use evren_agent::prompt_frame::{delivery_summary, is_execution_followup, PromptFrame};
use evren_agent::sessions::list_sessions;
use evren_agent::{AgentEvent, AgentSession};
use evren_api::{ensure_terms_accepted, mask_key, ApiClient, TermsOptions, TermsUi};
use evren_core::config::Config;
use evren_core::error::EvrenError;
use evren_core::models::RECOMMENDED_MODELS;
use evren_core::texts;
use evren_core::tools::file_ops::{resolve_workspace_path, rollback_last_backup};
use evren_core::tools::Mode;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::commands::{canonical, is_command_input, known_commands, unknown_command_message};
use crate::events::{EventPrinter, StdinPrompter};
use crate::extras;
use crate::handlers;
use crate::input::{make_reader, EscWatcher, InputQueue, LineReader, ReadResult, WatchPause};
use crate::shell;
use crate::ui::{self, print_error, print_info, print_success, print_warning, thousands};

pub const FORM_INTRO: &str =
    "Brief yaz ve Enter'a bas; ayrıntı ve komutlar için /help. Slash komutunu da doğrudan buraya yaz.";

const INTERRUPT_FOLLOWUP: &str =
    "Geçmiş duruyor. Düzeltme yazıp Enter'a basın; boş Enter kaldığı yerden devam eder.";

pub const HELP_TEXT: &str = "
EVREN CLI Komutları:
  /help         - Yardım menüsü
  /version      - Sürüm bilgisi ve sürüm notları
  /model [ad]   - Model değiştir
  /mode <kip>   - Çalışma kipi (normal|ask|plan)
  /effort <s>   - Düşünme yoğunluğu (low|medium|high|none)
  /models       - Modelleri listele
  /quota        - Kalan token kotası
  /tokens       - Bağlam/token bütçesi raporu
  /debug [N]    - Son görevin adım tablosu (sondan N. görev)
  !komut        - Kabuk komutu çalıştır (Git Bash); çıktı ajana bağlam olur
  /steps [n]    - Adım tavanını göster veya ayarla (tasarruf: düşük tut)
  /keys         - API anahtar havuzunu yönet
  /keys next    - Sıradaki API anahtarına geç
  /terms        - Şartları kontrol et / kabul et
  /skills       - Mühendislik becerilerini listele
  /docs         - agent.md / changelog.md bağlam dosyaları
  /skill <b>    - Bir beceriyi uygula (/apply eşdeğeri)
  /diagnose     - Arıza triyajı çalıştır
  /template     - Çıktı şablonunu doldur
  /memory       - Proje hafızasını göster/yönet
  /track        - Görevleri listele
  /task add     - Görev ekle
  /progress     - Görev ilerlemesini güncelle
  /playbook     - Playbook listele/başlat
  /resume       - Playbook adımından devam et
  /language     - Çıktı dilini ayarla
  /scan         - Proje dosya ağacını tara
  /files        - Aktif bağlam dosyaları
  /add <dosya>  - Dosya bağlamı ekle
  /drop <dosya> - Dosya bağlamı çıkar
  /undo <dosya> - Yedeği geri yükle (rollback)
  /run <komut>  - Terminal komutu çalıştır
  /todos        - Ajanın güncel görev listesini göster
  /ssh          - SSH host'larını listele/yönet
  /update       - En son sürümü çek ve yeniden başlat
  /clear        - Geçmişi temizle
  /exit         - Çıkış
";

/// Faz 4'e kalan komutlar (beceriler, takip, SSH, ekip, güncelleme).
const PHASE4_COMMANDS: &[&str] = &[
    "/skills",
    "/apply",
    "/skill",
    "/playbook",
    "/diagnose",
    "/template",
    "/track",
    "/task",
    "/progress",
    "/checkpoint",
    "/blockers",
    "/check-in",
    "/decisions",
    "/ssh",
    "/iterate",
    "/iterasyon",
    "/agent",
    "/agents",
    "/team",
    "/update",
];

/// Faz 3'ün sonraki adımlarında taşınacak çekirdek komutlar.
const PENDING_COMMANDS: &[&str] = &["/docs"];

pub fn phase4_message(cmd: &str) -> String {
    format!("{cmd} komutu Faz 4'te gelecek; bu sürümde henüz yok.")
}

/// REPL başlatma seçenekleri (`evren` argümansız).
#[derive(Debug, Clone, Default)]
pub struct ReplOptions {
    pub model: Option<String>,
    pub continue_last: bool,
}

struct TuiTermsUi;

impl TermsUi for TuiTermsUi {
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
        extras::prompt_confirm(msg)
    }
}

/// Kota satırı: `kalan / limit (anahtar ****)`.
fn quota_text(data: &serde_json::Map<String, Value>, masked: Option<String>) -> String {
    let remaining = data
        .get("remaining")
        .or_else(|| data.get("remaining_daily_tokens"));
    let limit = data.get("limit").and_then(Value::as_i64);
    let mut text = match (remaining.and_then(Value::as_i64), limit) {
        (Some(r), Some(l)) => format!("{} / {}", thousands(r), thousands(l)),
        (Some(r), None) => thousands(r),
        _ => match remaining {
            Some(Value::String(s)) => s.clone(),
            Some(v) if !v.is_null() => v.to_string(),
            _ => "Bilinmiyor".to_string(),
        },
    };
    if let Some(m) = masked {
        text.push_str(&format!(" (anahtar {m})"));
    }
    text
}

/// Turda çalışacak iş.
enum Turn {
    Step(String),
    Continue,
    Execute(String),
}

/// Kullanıcı girdisinin sonucu.
enum Flow {
    Continue,
    Exit,
}

struct Repl {
    session: Option<AgentSession>,
    rx: mpsc::UnboundedReceiver<AgentEvent>,
    printer: EventPrinter,
    reader: Box<dyn LineReader + Send>,
    pause: WatchPause,
    queue: InputQueue,
    workspace: PathBuf,
    /// Önceki tur ESC/Ctrl+C ile kesildi; sonraki girdi düzeltmedir.
    interrupted: bool,
    /// `.evren/commands/*.md` özel komutları.
    custom: BTreeMap<String, extras::CustomCommand>,
}

/// Argümansız `evren`: etkileşimli REPL'i açar.
pub async fn run(
    config: &Config,
    workspace: &Path,
    opts: ReplOptions,
) -> Result<ExitCode, EvrenError> {
    let evren_dir = workspace.join(".evren");
    let _ = std::fs::create_dir_all(&evren_dir);
    if config.flag("EVREN_AUTO_DOCS", true) {
        print_info("Bağlam: atlandı (agent.md/changelog.md önyüklemesi Rust sürümünde henüz yok)");
    } else {
        print_info("Bağlam: kapalı (EVREN_AUTO_DOCS=false)");
    }

    let client = Arc::new(ApiClient::new(config)?);
    // Havuzda birden çok anahtar varsa kotası en yüksek olan sabitlenir;
    // şartlar ve kota o anahtarla sorulur.
    let multi_key = client.with_pool(|p| p.len()) > 1;
    let best_quota = if multi_key {
        tokio::time::timeout(Duration::from_secs(5), client.select_best_key(None))
            .await
            .ok()
            .and_then(Result::ok)
            .map(|b| b.quota)
    } else {
        None
    };
    let mut terms_ui = TuiTermsUi;
    let _ = ensure_terms_accepted(
        &client,
        None,
        TermsOptions {
            interactive: true,
            force: false,
            silent_ok: true,
        },
        &mut terms_ui,
    )
    .await;

    let data = match best_quota {
        Some(q) => Some(q),
        None => tokio::time::timeout(Duration::from_secs(5), client.get_quota(None))
            .await
            .ok(),
    };
    let quota = match data {
        Some(data) => {
            let masked = if multi_key {
                client.current_key().map(|k| mask_key(&k))
            } else {
                None
            };
            quota_text(&data, masked)
        }
        None => "kontrol ediliyor...".to_string(),
    };

    let (tx, rx) = mpsc::unbounded_channel();
    let mut session = AgentSession::new(
        client,
        config.clone(),
        workspace,
        tx,
        CancellationToken::new(),
    );
    if let Some(m) = &opts.model {
        session.set_model(m);
    }
    ui::print_banner(
        evren_core::VERSION,
        session.model(),
        workspace,
        &quota,
        &config.source_report(),
        &ui::release_notes(4)
            .iter()
            .map(|n| ui::clean_release_note(n, 56))
            .collect::<Vec<_>>(),
    );
    print_info(FORM_INTRO);
    if opts.continue_last {
        match list_sessions(workspace, 1).first() {
            Some(last) if session.load_session(last) => {
                let stem = last
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                print_info(&format!("Son oturum yüklendi: {stem}"));
            }
            _ => print_info("Devam ettirilecek oturum yok."),
        }
    }

    let pause = WatchPause::default();
    let mut repl = Repl {
        session: Some(session),
        rx,
        printer: EventPrinter::new(
            workspace.to_path_buf(),
            Box::new(StdinPrompter::new(pause.clone())),
        ),
        reader: make_reader(&evren_dir.join("history.txt")),
        pause,
        queue: Arc::new(Mutex::new(VecDeque::new())),
        workspace: workspace.to_path_buf(),
        interrupted: false,
        custom: extras::refresh_custom_commands(workspace),
    };
    repl.run_loop().await;
    repl.shutdown();
    Ok(ExitCode::SUCCESS)
}

impl Repl {
    fn session(&mut self) -> Option<&mut AgentSession> {
        self.session.as_mut()
    }

    fn read(&mut self, label: &str) -> ReadResult {
        let reader = &mut self.reader;
        tokio::task::block_in_place(|| reader.read_line(label))
    }

    fn pop_queued(&self) -> Option<String> {
        self.queue.lock().ok().and_then(|mut q| q.pop_front())
    }

    async fn run_loop(&mut self) {
        loop {
            let Some(session) = self.session.as_ref() else {
                return;
            };
            if let Some(queued) = self.pop_queued() {
                print_info(&format!("Sıradaki istek çalıştırılıyor: {queued}"));
                if let Flow::Exit = self.dispatch(queued).await {
                    return;
                }
                continue;
            }
            let mode = session.mode();
            let tag = if mode == Mode::Normal {
                String::new()
            } else {
                format!(" ({})", mode.as_str())
            };
            println!("evren [{}]{tag}", session.model());
            let label = if self.interrupted {
                print_info(INTERRUPT_FOLLOWUP);
                "Düzeltme > "
            } else {
                "Brief > "
            };
            let line = match self.read(label) {
                ReadResult::Line(l) => l,
                ReadResult::Interrupted => {
                    println!("\nİşlem durduruldu. Çıkmak için /exit yazabilirsiniz.");
                    continue;
                }
                ReadResult::Eof => {
                    println!("\nÇıkış yapılıyor...");
                    return;
                }
            };
            if self.interrupted {
                self.interrupted = false;
                let text = line.trim().to_string();
                if text.is_empty() {
                    self.run_turn(Turn::Continue).await;
                    continue;
                }
                if !is_command_input(&text) {
                    self.run_turn(Turn::Step(text)).await;
                    continue;
                }
            }
            if let Flow::Exit = self.dispatch(line).await {
                return;
            }
        }
    }

    /// Tek bir girdiyi işler: slash komutu, devam ya da brief.
    async fn dispatch(&mut self, line: String) -> Flow {
        let text = line.trim().to_string();
        if text.is_empty() {
            return Flow::Continue;
        }
        if text.to_lowercase() == "devam" {
            return self.command("/continue").await;
        }
        if shell::is_shell_input(&text) {
            let Some(session) = self.session.as_mut() else {
                return Flow::Exit;
            };
            shell::run_user_shell(&text, session, shell::SHELL_TIMEOUT).await;
            return Flow::Continue;
        }
        if is_command_input(&text) {
            return self.command(&text).await;
        }
        if is_execution_followup(&text) {
            self.run_turn(Turn::Execute(text)).await;
            return Flow::Continue;
        }
        let Some(session) = self.session() else {
            return Flow::Exit;
        };
        let frame = PromptFrame {
            brief: session.compact_user_text(&text),
            ..PromptFrame::default()
        };
        print_info(&format!(
            "Kalıp API'ye ayrıldı: {}",
            delivery_summary(&frame)
        ));
        let content = session.apply_frame(&frame, "", true);
        self.run_turn(Turn::Step(content)).await;
        Flow::Continue
    }

    /// Ajan turunu çalıştırır; olayları basar, ESC/Ctrl+C turu iptal eder.
    async fn run_turn(&mut self, turn: Turn) {
        let Some(mut session) = self.session.take() else {
            return;
        };
        let cancel = session.reset_cancel();
        let watcher = EscWatcher::start(cancel.clone(), self.pause.clone(), self.queue.clone());
        let mut worker = tokio::spawn(async move {
            let result = match turn {
                Turn::Step(text) => session.run_step(&text, None, true).await,
                Turn::Continue => session.continue_task(None).await,
                Turn::Execute(text) => session.execute_followup(&text, None).await,
            };
            (session, result)
        });
        let joined = loop {
            tokio::select! {
                Some(event) = self.rx.recv() => self.printer.handle(event),
                res = &mut worker => break res,
                _ = tokio::signal::ctrl_c() => cancel.cancel(),
            }
        };
        watcher.finish();
        while let Ok(event) = self.rx.try_recv() {
            self.printer.handle(event);
        }
        self.printer.finish();
        let (mut session, result) = match joined {
            Ok(pair) => pair,
            Err(e) => {
                print_error(&format!("Beklenmeyen hata: {e}"));
                return;
            }
        };
        if result.starts_with("İPTAL:") || cancel.is_cancelled() {
            println!("\nİşlem durduruldu. Geçmiş duruyor; düzeltme yazarak devam edebilirsiniz.");
            self.interrupted = true;
        } else if result.starts_with("HATA:") {
            print_error(&result);
        }
        if self.printer.settings_dirty {
            session.reload_settings();
            self.printer.settings_dirty = false;
        }
        session.save_session();
        self.session = Some(session);
    }

    async fn command(&mut self, text: &str) -> Flow {
        let (typed, arg) = match text.split_once(char::is_whitespace) {
            Some((c, a)) => (c.to_string(), a.trim().to_string()),
            None => (text.to_string(), String::new()),
        };
        let lowered = typed.to_lowercase();
        if !known_commands().contains(&lowered.as_str()) {
            if let Some(entry) = self.custom.get(&lowered) {
                let prompt = extras::render_custom_command(entry, &arg);
                self.run_turn(Turn::Step(prompt)).await;
                return Flow::Continue;
            }
        }
        let cmd = canonical(&typed);
        let Some(session) = self.session.as_mut() else {
            return Flow::Exit;
        };
        match cmd.as_str() {
            "/exit" => {
                print_info("EVREN CLI sonlandırılıyor. İyi çalışmalar!");
                return Flow::Exit;
            }
            "/help" => println!("{HELP_TEXT}"),
            "/version" => {
                let ver = evren_core::VERSION;
                print_info(&format!("EVREN CLI Sürümü: v{ver}"));
                let notes = ui::release_notes(20);
                if !notes.is_empty() {
                    println!(
                        "
v{ver} Sürüm Notları:"
                    );
                    for note in notes {
                        println!("  • {note}");
                    }
                    println!();
                }
            }
            "/model" => {
                if arg.is_empty() {
                    let current = session.model().to_string();
                    print_info(&format!("Aktif Model: {current}"));
                    println!("\nÖnerilen Modeller:");
                    for (id, desc) in RECOMMENDED_MODELS {
                        let mark = if id == current { " ✔" } else { "" };
                        println!("  • {id:<22} : {desc}{mark}");
                    }
                    println!("\nModel değiştirmek için: /model <model_adı>");
                } else {
                    session.set_model(&arg);
                    print_success(&format!("Model değiştirildi: {arg}"));
                }
            }
            "/mode" => {
                if arg.is_empty() {
                    print_info(&format!("Aktif Kip: {}", mode_label(session.mode())));
                    println!("  • normal : Tüm araçlar açık");
                    println!("  • ask    : Salt-okuma (yazma kapalı)");
                    println!("  • plan   : Yalnızca plan yazımı");
                    println!("\nDeğiştirmek için: /mode <normal|ask|plan>");
                } else if let Some(mode) = Mode::parse(&arg.to_lowercase()) {
                    session.set_mode(mode);
                    print_success(&format!("Çalışma kipi değiştirildi: {}", mode_label(mode)));
                } else {
                    print_warning("Geçersiz kip. Kullanım: /mode <normal|ask|plan>");
                }
            }
            "/todos" => {
                let rendered = session.todos().render();
                if rendered.trim().is_empty() {
                    print_info("Görev listesi boş.");
                } else {
                    println!("{rendered}");
                }
            }
            "/bg" => self.background(&arg),
            "/clear" => {
                extras::clear_checkpoint(&self.workspace);
                session.clear_history();
                print_success("Sohbet geçmişi, kalıp ve aktif bağlam temizlendi.");
            }
            "/undo" => {
                if arg.is_empty() {
                    print_warning("Kullanım: /undo <dosya_yolu>");
                } else {
                    let target = resolve_workspace_path(&arg, &self.workspace);
                    let (ok, msg) = rollback_last_backup(&target, &self.workspace);
                    if ok {
                        print_success(&msg);
                    } else {
                        print_error(&msg);
                    }
                }
            }
            "/auto" => match arg.to_lowercase().as_str() {
                "" => {
                    let state = if session.auto_approve() {
                        "AÇIK (Değişiklikler otomatik uygulanır)"
                    } else {
                        "KAPALI (Her değişiklikte [Y/n] onayı istenir)"
                    };
                    print_info(&format!("Otomatik Onay Modu: {state}"));
                    println!("Değiştirmek için: /auto on veya /auto off");
                }
                "on" | "true" | "1" | "ac" | "aç" => {
                    session.set_auto_approve(true);
                    print_success("Otomatik onay modu AÇILDI. Kod değişiklikleri otomatik uygulanacak (yedekleme varsayılan olarak kapalı; --backup ile açılır).");
                }
                "off" | "false" | "0" | "kapat" => {
                    session.set_auto_approve(false);
                    print_warning("Otomatik onay modu KAPATILDI. Her dosya değişikliğinde kullanıcı onayı istenecek.");
                }
                _ => print_warning("Kullanım: /auto [on|off]"),
            },
            "/continue" => self.run_turn(Turn::Continue).await,
            "/execute" => {
                if arg.is_empty() {
                    print_warning("Kullanım: /execute <onay veya takip yanıtı>");
                } else {
                    self.run_turn(Turn::Execute(arg)).await;
                }
            }
            "/effort" => handlers::effort(session, &arg),
            "/models" => handlers::models(session).await,
            "/quota" => handlers::quota(session).await,
            "/terms" => handlers::terms(session, &arg, &mut TuiTermsUi).await,
            "/tokens" => handlers::tokens(session),
            "/steps" => handlers::steps(session, &arg),
            "/rol" => extras::role(session, &arg),
            "/commit" => extras::commit(session).await,
            "/review" => {
                if extras::review_ready(&self.workspace) {
                    self.run_turn(Turn::Step(extras::REVIEW_PROMPT.into())).await;
                }
            }
            "/init" => {
                if extras::init_ready(&self.workspace) {
                    self.run_turn(Turn::Step(extras::INIT_PROMPT.into())).await;
                }
            }
            "/context" => extras::context(session),
            "/compact" => extras::compact(session, &arg),
            "/rewind" => extras::rewind(session),
            "/doctor" => extras::doctor(session).await,
            "/keys" => extras::keys(session, &arg),
            "/debug" => extras::debug(session, &arg),
            "/memory" => extras::memory(session, &arg),
            "/language" => extras::language(session, &arg),
            "/scan" => extras::scan(&self.workspace, &arg),
            "/files" => extras::files(session),
            "/add" => extras::add(session, &arg),
            "/drop" => extras::drop(session, &arg),
            "/resume" if arg.to_lowercase() != "playbook" => extras::resume(session),
            "/resume" => print_warning(&phase4_message("/resume playbook")),
            "/run" => self.run_command(&arg).await,
            c if PHASE4_COMMANDS.contains(&c) => print_warning(&phase4_message(c)),
            c if PENDING_COMMANDS.contains(&c) => print_warning(texts::NOT_PORTED_YET),
            _ => print_warning(&unknown_command_message(&typed)),
        }
        Flow::Continue
    }

    /// `/run <komut>`: onay istemi olayları basılırken komut çalışır.
    async fn run_command(&mut self, arg: &str) {
        if arg.is_empty() {
            print_warning("Kullanım: /run <komut>");
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.reset_cancel();
        let ctx = session.tool_ctx().clone();
        let auto = session.auto_approve();
        let cmd = arg.to_string();
        let mut worker = tokio::task::spawn_blocking(move || {
            evren_core::tools::command::tool_run_command(&ctx, &cmd, auto, None, false, false)
        });
        let joined = loop {
            tokio::select! {
                Some(event) = self.rx.recv() => self.printer.handle(event),
                res = &mut worker => break res,
            }
        };
        while let Ok(event) = self.rx.try_recv() {
            self.printer.handle(event);
        }
        self.printer.finish();
        match joined {
            Ok(out) => println!("{out}"),
            Err(e) => print_error(&format!("Beklenmeyen hata: {e}")),
        }
    }

    fn background(&mut self, arg: &str) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let ctx = session.tool_ctx().clone();
        let Ok(mut bg) = ctx.background.lock() else {
            return;
        };
        let parts: Vec<&str> = arg.split_whitespace().collect();
        if parts.first().map(|p| p.to_lowercase()) == Some("stop".into()) {
            match parts.get(1) {
                None => print_warning("Kullanım: /bg stop <id>"),
                Some(id) if bg.stop(id) => {
                    print_success(&format!("Arka plan komutu durduruldu: {id}"));
                }
                Some(id) => print_warning(&format!("Bilinmeyen arka plan id: {id}")),
            }
            return;
        }
        let list = bg.list();
        if list.is_empty() {
            print_info("Arka planda çalıştırılmış komut yok. Ajan run_command(run_in_background=true) ile başlatabilir.");
            return;
        }
        println!("\nArka Plan Komutları:");
        for p in list {
            let cmd: String = p.command.chars().take(70).collect();
            println!(
                "  {:<6} {:<28} {:>5.0}s  {cmd}",
                p.id,
                p.status_line(),
                p.elapsed_seconds()
            );
        }
        println!("  Durdurmak için: /bg stop <id>\n");
    }

    fn shutdown(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.save_session();
        let ctx = session.tool_ctx().clone();
        let killed = match ctx.background.lock() {
            Ok(mut bg) => bg.cleanup_all(),
            Err(_) => 0,
        };
        if killed > 0 {
            print_info(&format!(
                "Arka planda çalışan {killed} komut kapatılırken durduruldu."
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quota_formats() {
        let m = json!({"remaining": 10, "limit": 100});
        let m = m.as_object().cloned().unwrap_or_default();
        assert_eq!(quota_text(&m, None), "10 / 100");
        let m = json!({"remaining_daily_tokens": 12345});
        let m = m.as_object().cloned().unwrap_or_default();
        assert_eq!(
            quota_text(&m, Some("ab***yz".into())),
            "12,345 (anahtar ab***yz)"
        );
    }

    #[test]
    fn phase4_and_unknown_messages() {
        assert!(phase4_message("/skills").contains("Faz 4'te"));
        assert!(unknown_command_message("/xyz").starts_with("Bilinmeyen komut: '/xyz'."));
        for (cmd, _) in crate::commands::COMMAND_ALIASES {
            let handled = matches!(
                *cmd,
                "/exit"
                    | "/help"
                    | "/version"
                    | "/model"
                    | "/mode"
                    | "/todos"
                    | "/bg"
                    | "/clear"
                    | "/undo"
                    | "/auto"
                    | "/continue"
                    | "/execute"
                    | "/effort"
                    | "/models"
                    | "/quota"
                    | "/terms"
                    | "/tokens"
                    | "/steps"
                    | "/rol"
                    | "/commit"
                    | "/review"
                    | "/init"
                    | "/context"
                    | "/compact"
                    | "/rewind"
                    | "/doctor"
                    | "/keys"
                    | "/debug"
                    | "/memory"
                    | "/language"
                    | "/scan"
                    | "/files"
                    | "/add"
                    | "/drop"
                    | "/resume"
                    | "/run"
            );
            assert!(
                handled || PHASE4_COMMANDS.contains(cmd) || PENDING_COMMANDS.contains(cmd),
                "{cmd}"
            );
        }
        assert!(HELP_TEXT.contains("/exit         - Çıkış"));
    }
}
