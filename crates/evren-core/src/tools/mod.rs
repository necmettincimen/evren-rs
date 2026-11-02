//! Ajan araçları (Python `tools.py` ve yardımcı modülleri).
//!
//! - [`registry`][]: araç şeması, mod filtreleri, ad takma adları
//! - [`args`][]: model argümanlarını şemaya göre normalleştirme
//! - [`file_ops`][]: güvenli yol/okuma/yazma/yedek
//! - [`diff`][]: satır farkı ve snippet değiştirme
//! - [`proc`][], [`background`][]: süreç ağacı yönetimi
//! - [`command`][], [`fs_tools`][], [`patch`][], [`clone_tree`][]: araç gövdeleri
//! - [`vision`][]: görsel betimleme ve anlamsal kod arama
//! - [`dispatch`][]: `execute_tool_call`

pub mod args;
pub mod background;
pub mod clone_tree;
pub mod command;
pub mod diff;
pub mod dispatch;
pub mod file_ops;
pub mod fs_tools;
pub mod patch;
pub mod proc;
pub mod registry;
pub mod roots;
pub mod ssh;
pub mod vision;
pub mod web_fetch;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::secret_vault::SecretVault;

use crate::config::{Config, SslVerify};
use background::BackgroundManager;
use proc::StreamTag;

pub use dispatch::execute_tool_call;
pub use registry::{canonical_tool_name, tools_for_mode, tools_schema, Mode};

/// Onay türü (`request_approval(kind=...)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalKind {
    Edit,
    Write,
    Command,
    Background,
    Plan,
    SshRun,
    SshWrite,
}

/// Düzenleme önerisi: onay arayüzü eski/yeni içeriği gösterebilir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub path: String,
    pub old: String,
    pub new: String,
}

/// Kullanıcıya sorulan onay.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub kind: ApprovalKind,
    /// Onay arayüzü kategorisi: `edit` | `command` | `other` | `ssh`.
    pub ui_kind: &'static str,
    pub title: String,
    pub detail: String,
    pub default_yes: bool,
    pub proposal: Option<Proposal>,
    /// Onaydan önce gösterilen uyarı (`print_warning`), varsa.
    pub warning: Option<String>,
}

/// Onay cevabı: karar ve (edit/write için) kullanıcının düzenlediği içerik
/// (Python `confirm_action`: `decision in (allow, always)` + `edited_content`).
#[derive(Debug, Clone, Default)]
pub struct ApprovalReply {
    pub approved: bool,
    pub edited_content: Option<String>,
}

impl ApprovalReply {
    pub fn allow() -> Self {
        ApprovalReply {
            approved: true,
            edited_content: None,
        }
    }

    pub fn deny() -> Self {
        ApprovalReply::default()
    }
}

impl From<bool> for ApprovalReply {
    fn from(approved: bool) -> Self {
        ApprovalReply {
            approved,
            edited_content: None,
        }
    }
}

/// Onay cevabı veren geri çağrı.
pub type Approver = Box<dyn Fn(&ApprovalRequest) -> ApprovalReply + Send + Sync>;
/// Canlı komut çıktısı alıcısı.
pub type OutputSink = Box<dyn Fn(StreamTag, &str) + Send + Sync>;
/// Bilgi/diff satırı alıcısı (`print_info`, `print_diff`).
pub type InfoSink = Box<dyn Fn(&str) + Send + Sync>;

/// Araçların paylaştığı bağlam: çalışma dizini, onay kapısı, çıktı kanalı,
/// arka plan süreç yöneticisi ve kabuk seçimi.
pub struct ToolContext {
    pub root: PathBuf,
    /// `auto_approve` çağrı parametresi ya da `EVREN_AUTO_APPROVE`.
    pub auto_approve: bool,
    /// `bash` | `cmd` | `powershell` (Windows'ta geçerli).
    pub windows_shell: &'static str,
    pub git_bash: Option<PathBuf>,
    /// `use_python=true` için yorumlayıcı (Python `sys.executable`).
    pub python_exe: String,
    /// `web_fetch` TLS ayarı (`EVREN_CA_BUNDLE` / `EVREN_SSL_VERIFY`).
    pub ssl: SslVerify,
    /// `search_code` / görsel betimleme API ayarları.
    pub vision: vision::VisionSettings,
    pub background: Mutex<BackgroundManager>,
    pub approver: Option<Approver>,
    pub output: Option<OutputSink>,
    pub info: Option<InfoSink>,
    /// Oturumun sır kasası: komutlar çalıştırılmadan önce `{{SECRET_n}}`
    /// yer tutucuları açılır, akan ve dönen çıktı yeniden maskelenir.
    pub vault: Option<Arc<Mutex<SecretVault>>>,
}

impl ToolContext {
    /// Bu çağrının kökü: `--root` ile seçilen ek kök ya da ana çalışma dizini.
    pub fn root(&self) -> PathBuf {
        roots::current_override().unwrap_or_else(|| self.root.clone())
    }

    /// Verilen yapılandırmayla bağlam; onay geri çağrısı yoksa her şey onaylanır.
    pub fn new(root: &Path, config: &Config) -> Self {
        ToolContext {
            root: file_ops::resolve_path(root),
            auto_approve: config.auto_approve(),
            windows_shell: config.windows_shell(),
            git_bash: config.git_bash_path(),
            python_exe: config
                .get("EVREN_PYTHON")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("python")
                .to_string(),
            ssl: config.ssl_verify(),
            vision: vision::VisionSettings::from_config(config),
            background: Mutex::new(BackgroundManager::new()),
            approver: None,
            output: None,
            info: None,
            vault: None,
        }
    }

    /// Süreç ortamıyla (`Config::load`) bağlam.
    pub fn load(root: &Path) -> Self {
        Self::new(root, &Config::load())
    }

    /// Her şeyi onaylayan test/otomasyon bağlamı: `cmd` kabuğu, otomatik onay.
    pub fn auto(root: &Path) -> Self {
        let mut env = std::collections::BTreeMap::new();
        env.insert("EVREN_SHELL".to_string(), "cmd".to_string());
        env.insert("EVREN_AUTO_APPROVE".to_string(), "true".to_string());
        if let Some(p) = std::env::var_os("PATH") {
            env.insert("PATH".to_string(), p.to_string_lossy().into_owned());
        }
        Self::new(root, &Config::from_env(env))
    }

    pub fn with_approver(mut self, approver: Approver) -> Self {
        self.approver = Some(approver);
        self
    }

    pub fn with_output(mut self, sink: OutputSink) -> Self {
        self.output = Some(sink);
        self
    }

    pub fn with_info(mut self, sink: InfoSink) -> Self {
        self.info = Some(sink);
        self
    }

    pub fn with_vault(mut self, vault: Arc<Mutex<SecretVault>>) -> Self {
        self.vault = Some(vault);
        self
    }

    /// Kasa doluysa `f`'i kilit altında çalıştırır; kasa yoksa ya da boşsa `None`.
    fn with_secrets<R>(&self, f: impl FnOnce(&SecretVault) -> R) -> Option<R> {
        let vault = self.vault.as_ref()?;
        let guard = vault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (!guard.is_empty()).then(|| f(&guard))
    }

    /// Yer tutucuları gerçek değerlere açar (yalnız çalıştırma için).
    pub fn expand_secrets(&self, text: &str) -> String {
        self.with_secrets(|v| v.expand(text))
            .unwrap_or_else(|| text.to_string())
    }

    /// Bilinen sır değerlerini yer tutucuyla değiştirir.
    pub fn mask_secrets(&self, text: &str) -> String {
        self.with_secrets(|v| v.mask(text))
            .unwrap_or_else(|| text.to_string())
    }

    /// Alt süreçler için `EVREN_SECRET_n` ortam değişkenleri.
    pub fn secret_env(&self) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = self
            .with_secrets(|v| v.env().into_iter().collect())
            .unwrap_or_default();
        env.sort();
        env
    }

    pub(crate) fn emit_info(&self, text: &str) {
        if let Some(sink) = &self.info {
            sink(text);
        }
    }

    pub(crate) fn emit_output(&self, tag: StreamTag, line: &str) {
        if let Some(sink) = &self.output {
            sink(tag, &self.mask_secrets(line));
        }
    }

    /// Onay kapısı: onaylandıysa `None`, aksi halde modele dönecek ret metni.
    pub fn request_approval(&self, auto_approve: bool, req: ApprovalRequest) -> Option<String> {
        self.request_approval_full(auto_approve, req).err()
    }

    /// Onay kapısı (tam cevap): onaylandıysa `Ok(düzenlenmiş içerik)`,
    /// reddedildiyse `Err(ret metni)`. Otomatik onayda ya da onay geri
    /// çağrısı yokken `Ok(None)`.
    pub fn request_approval_full(
        &self,
        auto_approve: bool,
        req: ApprovalRequest,
    ) -> Result<Option<String>, String> {
        if auto_approve || self.auto_approve {
            return Ok(None);
        }
        let Some(approver) = &self.approver else {
            return Ok(None);
        };
        if let Some(w) = &req.warning {
            self.emit_info(w);
        }
        let deny = deny_text(req.kind);
        let reply = approver(&req);
        if reply.approved {
            Ok(reply.edited_content)
        } else {
            Err(deny.to_string())
        }
    }
}

/// Reddedilen onayın model metni (`request_approval` içindeki `deny`).
pub fn deny_text(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::Edit => {
            "İPTAL EDİLDİ: Kullanıcı bu değişikliği onaylamadı. Lütfen aynı değişikliği tekrar denemeyin ve kullanıcıya ne yapmak istediğini sorun."
        }
        ApprovalKind::Write => {
            "İPTAL EDİLDİ: Kullanıcı dosya yazma işlemini onaylamadı. Lütfen aynı işlemi tekrar denemeyin."
        }
        ApprovalKind::Command => "İPTAL EDİLDİ: Kullanıcı terminal komutunun çalıştırılmasını reddetti.",
        ApprovalKind::Background => "İPTAL EDİLDİ: Kullanıcı arka plan komutunun çalıştırılmasını reddetti.",
        ApprovalKind::Plan => "İPTAL EDİLDİ: Kullanıcı plan kaydını onaylamadı.",
        ApprovalKind::SshRun => "İPTAL EDİLDİ: Kullanıcı uzak komutun çalıştırılmasını reddetti.",
        ApprovalKind::SshWrite => "İPTAL EDİLDİ: Kullanıcı uzak dosya yazma işlemini onaylamadı.",
    }
}

/// `request_approval("edit", path=..., proposal=...)`.
pub fn edit_request(path: &str, proposal: Option<Proposal>) -> ApprovalRequest {
    ApprovalRequest {
        kind: ApprovalKind::Edit,
        ui_kind: "edit",
        title: format!("'{path}' dosyasındaki değişiklik uygulansın mı?"),
        detail: path.to_string(),
        default_yes: true,
        proposal,
        warning: None,
    }
}

/// `request_approval("write", path=..., action=...)`.
pub fn write_request(path: &str, action: &str, proposal: Option<Proposal>) -> ApprovalRequest {
    ApprovalRequest {
        kind: ApprovalKind::Write,
        ui_kind: "edit",
        title: format!("'{path}' dosyası {action} mı?"),
        detail: path.to_string(),
        default_yes: true,
        proposal,
        warning: None,
    }
}

/// `request_approval("command", shown=...)`.
pub fn command_request(shown: &str) -> ApprovalRequest {
    ApprovalRequest {
        kind: ApprovalKind::Command,
        ui_kind: "command",
        title: "Bu komutun çalıştırılmasına izin veriyor musunuz?".to_string(),
        detail: shown.to_string(),
        default_yes: false,
        proposal: None,
        warning: Some(format!(
            "Ajan terminal komutu çalıştırmak istiyor:\n  > {shown}"
        )),
    }
}

/// `request_approval("ssh_run", alias=..., shown=...)`.
pub fn ssh_run_request(alias: &str, shown: &str) -> ApprovalRequest {
    ApprovalRequest {
        kind: ApprovalKind::SshRun,
        ui_kind: "ssh",
        title: format!("'{alias}' sunucusunda bu komutun çalıştırılmasına izin veriyor musunuz?"),
        detail: format!("[{alias}] {shown}"),
        default_yes: false,
        proposal: None,
        warning: Some(format!(
            "Ajan uzak sunucuda komut çalıştırmak istiyor:
  [{alias}] > {shown}"
        )),
    }
}

/// `request_approval("ssh_write", alias=..., remote_path=..., size=...)`.
pub fn ssh_write_request(alias: &str, remote_path: &str, size: usize) -> ApprovalRequest {
    ApprovalRequest {
        kind: ApprovalKind::SshWrite,
        ui_kind: "ssh",
        title: format!("'{alias}:{remote_path}' uzak dosyası yazılsın mı?"),
        detail: format!("[{alias}] {remote_path} ({size} karakter)"),
        default_yes: false,
        proposal: None,
        warning: Some(format!(
            "Ajan uzak sunucuda dosya yazmak istiyor:
  [{alias}] > {remote_path} ({size} karakter)"
        )),
    }
}

/// `request_approval("background", shown=...)`.
pub fn background_request(shown: &str) -> ApprovalRequest {
    ApprovalRequest {
        kind: ApprovalKind::Background,
        ui_kind: "command",
        title: "Bu komutun arka planda çalıştırılmasına izin veriyor musunuz?".to_string(),
        detail: shown.to_string(),
        default_yes: false,
        proposal: None,
        warning: Some(format!(
            "Ajan arka planda terminal komutu çalıştırmak istiyor:\n  > {shown}"
        )),
    }
}

/// `request_approval("plan", path=...)`.
pub fn plan_request(path: &str) -> ApprovalRequest {
    ApprovalRequest {
        kind: ApprovalKind::Plan,
        ui_kind: "other",
        title: format!("'{path}' planı kaydedilsin mi?"),
        detail: path.to_string(),
        default_yes: true,
        proposal: None,
        warning: None,
    }
}

#[cfg(test)]
mod tests;
