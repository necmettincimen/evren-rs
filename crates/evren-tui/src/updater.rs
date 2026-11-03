//! Kendini güncelleme (Python `updater.py`, Rust kurulumuna uyarlanmış).
//!
//! `/update` ya da `evren update`:
//!
//!   1. İkilinin derlendiği kaynak bir git checkout'u ise onu kullanır
//!      (`git pull --ff-only origin <dal>`); değilse kalıcı bir kaynak klonunu
//!      (`%LOCALAPPDATA%/evren-rs/source`) ilk seferde sığ klonlar, sonra yalnızca
//!      yeni commit'leri çeker.
//!   2. Windows'ta çalışan `evren.exe` süreç içinde değiştirilemez: kurulum
//!      (`scripts/install.ps1`) bu süreç kapandıktan sonra ayrı pencerede çalışır.
//!      Diğer sistemlerde `cargo install --path crates/evren-cli` çalışır ve süreç
//!      yeni ikiliyle yeniden başlatılabilir.
//!
//! Tüm adımlar `<kaynak>/.evren/logs/update.log` dosyasına yazılır.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const REPO_OWNER: &str = "necmettincimen";
pub const REPO_NAME: &str = "evren-rs";
pub const SOURCE_BRANCH: &str = "main";

pub fn repo_git_url() -> String {
    format!("https://github.com/{REPO_OWNER}/{REPO_NAME}.git")
}

/// İkilinin derlendiği çalışma alanı kökü (derleme anındaki yol).
pub fn get_project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

/// Kök bir git çalışma kopyası mı (ve hâlâ bu projenin kaynağı mı).
pub fn is_git_checkout(root: &Path) -> bool {
    root.join(".git").exists() && root.join("Cargo.toml").is_file()
}

/// Kalıcı kaynak klonunun yolu.
pub fn get_source_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("evren-rs").join("source")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Güncellemenin kaynak dizini: checkout ya da kalıcı klon.
pub fn source_root() -> PathBuf {
    let root = get_project_root();
    if is_git_checkout(&root) {
        root
    } else {
        get_source_dir()
    }
}

/// Windows'ta kurulum süreç kapandıktan sonra ayrı pencerede yapılır.
pub fn update_is_deferred() -> bool {
    cfg!(windows)
}

fn log_path() -> PathBuf {
    let dir = get_project_root().join(".evren").join("logs");
    if std::fs::create_dir_all(&dir).is_ok() {
        return dir.join("update.log");
    }
    let fallback = std::env::temp_dir().join("evren-logs");
    let _ = std::fs::create_dir_all(&fallback);
    fallback.join("update.log")
}

fn log(message: &str) {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut fh) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = writeln!(fh, "[{stamp}] {message}");
    }
}

/// Komutu çalıştırır; (çıkış kodu, birleşik çıktı).
fn run(cmd: &[&str], cwd: Option<&Path>, timeout: Duration) -> (i32, String) {
    log(&format!(
        "RUN: {} (cwd={})",
        cmd.join(" "),
        cwd.map(|p| p.display().to_string())
            .unwrap_or_else(|| "None".into())
    ));
    let Some((program, args)) = cmd.split_first() else {
        return (-1, String::new());
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log(&format!("ERROR: komut bulunamadı: {e}"));
            return (127, format!("Komut bulunamadı: {program} ({e})"));
        }
        Err(e) => {
            log(&format!("ERROR: {e}"));
            return (-1, e.to_string());
        }
    };
    let out_reader = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });
    let err_reader = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                log(&format!("ERROR: {e}"));
                return (-1, e.to_string());
            }
        }
    };
    let collect = |h: Option<std::thread::JoinHandle<Vec<u8>>>| {
        h.and_then(|h| h.join().ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    let output = collect(out_reader) + &collect(err_reader);
    let Some(status) = status else {
        log("ERROR: zaman aşımı");
        return (-1, "İşlem zaman aşımına uğradı.".into());
    };
    let code = status.code().unwrap_or(-1);
    let head: String = output.trim().chars().take(2000).collect();
    log(&format!("EXIT {code}: {head}"));
    (code, output)
}

fn git_branch(root: &Path) -> String {
    let (code, out) = run(
        &["git", "rev-parse", "--abbrev-ref", "HEAD"],
        Some(root),
        Duration::from_secs(30),
    );
    let branch = out
        .trim()
        .lines()
        .last()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if code == 0 && !branch.is_empty() && branch != "HEAD" {
        branch
    } else {
        SOURCE_BRANCH.to_string()
    }
}

/// `evren update --check`: kaynak klonunu çekmeden uzaktaki yeni commit sayısını bildirir.
pub fn check_update() -> Result<String, String> {
    let mut root = get_project_root();
    if !is_git_checkout(&root) {
        root = get_source_dir();
    }
    if !is_git_checkout(&root) {
        return Ok("Kaynak klonu yok; `evren update` ilk seferde klonlar.".into());
    }
    let branch = git_branch(&root);
    let (code, out) = run(&["git", "fetch", "origin", &branch], Some(&root), Duration::from_secs(60));
    if code != 0 {
        return Err(format!("git fetch başarısız oldu:
{}", out.trim()));
    }
    let (code, out) = run(
        &["git", "rev-list", "--count", "HEAD..FETCH_HEAD"],
        Some(&root),
        Duration::from_secs(30),
    );
    let behind: u32 = out.trim().lines().last().unwrap_or("0").trim().parse().unwrap_or(0);
    if code != 0 || behind == 0 {
        return Ok("Zaten güncel.".into());
    }
    let (_, subj) = run(
        &["git", "log", "-1", "--format=%s", "FETCH_HEAD"],
        Some(&root),
        Duration::from_secs(30),
    );
    Ok(format!("{behind} yeni commit var: {}", subj.trim().lines().last().unwrap_or("")))
}

/// Git checkout'u en son sürüme çeker.
fn pull_checkout(root: &Path) -> Result<String, String> {
    let branch = git_branch(root);
    let (code, out) = run(
        &["git", "pull", "--ff-only", "origin", &branch],
        Some(root),
        Duration::from_secs(300),
    );
    if code != 0 {
        return Err(format!("git pull başarısız oldu:\n{}", out.trim()));
    }
    Ok(if out.trim().is_empty() {
        "Zaten güncel.".into()
    } else {
        out.trim().to_string()
    })
}

/// Kaynak klonunu son commit'e getirir (ilk seferde sığ klon).
pub fn sync_source_clone() -> Result<String, String> {
    let target = get_source_dir();
    let target_s = target.to_string_lossy().into_owned();
    let (action, code, out) = if target.join(".git").exists() {
        let (mut code, mut out) = run(
            &[
                "git",
                "-C",
                &target_s,
                "fetch",
                "--depth",
                "1",
                "origin",
                SOURCE_BRANCH,
            ],
            None,
            Duration::from_secs(300),
        );
        if code == 0 {
            (code, out) = run(
                &["git", "-C", &target_s, "reset", "--hard", "FETCH_HEAD"],
                None,
                Duration::from_secs(120),
            );
        }
        ("git fetch", code, out)
    } else {
        if target.exists() {
            let _ = std::fs::remove_dir_all(&target);
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let url = repo_git_url();
        let (code, out) = run(
            &[
                "git",
                "clone",
                "--depth",
                "1",
                "--branch",
                SOURCE_BRANCH,
                &url,
                &target_s,
            ],
            None,
            Duration::from_secs(600),
        );
        ("git clone", code, out)
    };
    if code != 0 {
        return Err(format!("{action} başarısız oldu:\n{}", out.trim()));
    }
    Ok(if out.trim().is_empty() {
        "Kaynak güncel.".into()
    } else {
        out.trim().to_string()
    })
}

/// Kaynağı günceller (checkout ya da klon); kaynak kökünü döndürür.
fn sync_source() -> Result<(PathBuf, String), String> {
    let root = get_project_root();
    if is_git_checkout(&root) {
        pull_checkout(&root).map(|msg| (root, msg))
    } else {
        sync_source_clone().map(|msg| (get_source_dir(), msg))
    }
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Çalışan (kilitli) `target\release\evren.exe`'yi `evren.exe.running-<pid>` adına taşır;
/// Windows çalışan ikilinin silinmesine değil yeniden adlandırılmasına izin verir.
fn set_aside_running_exe(root: &Path) -> Option<PathBuf> {
    let exe = root.join("target").join("release").join("evren.exe");
    if !exe.is_file() {
        return None;
    }
    if std::fs::OpenOptions::new().write(true).open(&exe).is_ok() {
        return None;
    }
    let same = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .zip(exe.canonicalize().ok())
        .is_some_and(|(a, b)| a == b);
    let tag = if same {
        std::process::id().to_string()
    } else {
        chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
    };
    let moved = exe.with_file_name(format!("evren.exe.running-{tag}"));
    match std::fs::rename(&exe, &moved) {
        Ok(()) => {
            log(&format!(
                "SET ASIDE: {} -> {}",
                exe.display(),
                moved.display()
            ));
            Some(moved)
        }
        Err(e) => {
            log(&format!("WARN: kilitli ikili taşınamadı: {e}"));
            None
        }
    }
}

/// Kurulumu bu süreç `evren.exe`'yi bıraktıktan sonra yeni pencerede çalıştırır.
fn schedule_deferred_install(root: &Path) -> Result<String, String> {
    let script = root.join("scripts").join("install.ps1");
    set_aside_running_exe(root);
    let log_file = log_path();
    let body = format!(
        "$ErrorActionPreference = 'Continue'\n\
         Wait-Process -Id {pid} -ErrorAction SilentlyContinue\n\
         Start-Sleep -Seconds 2\n\
         Write-Host 'EVREN CLI güncelleniyor...'\n\
         Add-Content -Path {log} -Value (\"[\" + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + \"] DEFERRED INSTALL: {script}\") -Encoding utf8\n\
         & powershell -NoProfile -ExecutionPolicy Bypass -File {script} 2>&1 | ForEach-Object {{ $line = \"$_\"; Write-Host $line; Add-Content -Path {log} -Value $line -Encoding utf8 }}\n\
         $code = $LASTEXITCODE\n\
         Add-Content -Path {log} -Value (\"[\" + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + \"] DEFERRED INSTALL: EXIT $code\") -Encoding utf8\n\
         if ($code -eq 0) {{ Write-Host 'Güncelleme tamamlandı; evren komutunu yeniden açabilirsiniz.' }} \
         else {{ Write-Host 'Güncelleme başarısız. Ayrıntılar için yukarıdaki kurulum çıktısına bakın.' }}\n\
         Read-Host \"Pencereyi kapatmak için Enter'a basın...\"\n\
         exit $code\n",
        pid = std::process::id(),
        script = ps_quote(&script.to_string_lossy()),
        log = ps_quote(&log_file.to_string_lossy()),
    );
    let ps1 = std::env::temp_dir().join(format!("evren-update-{}.ps1", std::process::id()));
    // PowerShell 5.1 BOM'suz UTF-8'i ANSI okur; Türkçe metin için BOM yazılır.
    std::fs::write(&ps1, format!("\u{feff}{body}"))
        .map_err(|e| format!("Ertelenmiş güncelleme başlatılamadı: {e}"))?;
    log(&format!("SCHEDULE DEFERRED: {}", script.display()));
    let mut command = Command::new("powershell");
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        &ps1.to_string_lossy(),
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        command.creation_flags(CREATE_NEW_CONSOLE);
    }
    match command.spawn() {
        Ok(_) => Ok(
            "Güncelleme ayrı pencerede, EVREN CLI kapandıktan sonra çalışacak. \
                     Tamamlandığında evren komutunu yeniden açın."
                .into(),
        ),
        Err(e) => {
            log(&format!("ERROR: ertelenmiş güncelleme başlatılamadı: {e}"));
            Err(format!("Ertelenmiş güncelleme başlatılamadı: {e}"))
        }
    }
}

/// `git status --porcelain` çıktısından takip edilen değişiklikleri sayar.
fn count_dirty(porcelain: &str) -> usize {
    porcelain
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("??"))
        .count()
}

/// Çalışma ağacında commit'lenmemiş değişiklik varsa güncelleme öncesi uyarı metni.
pub fn dirty_tree_warning() -> Option<String> {
    let root = get_project_root();
    if !is_git_checkout(&root) {
        return None;
    }
    let (code, out) = run(
        &["git", "status", "--porcelain"],
        Some(&root),
        Duration::from_secs(30),
    );
    let n = if code == 0 { count_dirty(&out) } else { 0 };
    if n == 0 {
        return None;
    }
    let msg = format!(
        "Çalışma ağacında {n} commit'lenmemiş değişiklik var ({}); \
         git pull çakışırsa güncelleme başarısız olur, derleme yerel değişiklikleri içerir.",
        root.display()
    );
    log(&format!("WARN: {msg}"));
    Some(msg)
}

/// Güncellemeyi yapar: (başarılı mı, mesaj). Yeniden başlatma çağırana kalır.
pub fn perform_update() -> (bool, String) {
    log("=== Güncelleme başlatıldı ===");
    let (root, msg) = match sync_source() {
        Ok(pair) => pair,
        Err(e) => {
            log("=== Güncelleme BAŞARISIZ ===");
            return (false, e);
        }
    };
    if update_is_deferred() {
        let result = schedule_deferred_install(&root);
        log(if result.is_ok() {
            "=== Güncelleme ertelendi ==="
        } else {
            "=== Güncelleme BAŞARISIZ ==="
        });
        return match result {
            Ok(m) => (true, m),
            Err(e) => (false, e),
        };
    }
    let crate_dir = root.join("crates").join("evren-cli");
    let (code, out) = run(
        &[
            "cargo",
            "install",
            "--locked",
            "--path",
            &crate_dir.to_string_lossy(),
        ],
        Some(&root),
        Duration::from_secs(1800),
    );
    if code != 0 {
        log("=== Güncelleme BAŞARISIZ ===");
        return (false, format!("Kurulum başarısız oldu:\n{}", out.trim()));
    }
    log("=== Güncelleme tamamlandı ===");
    (true, msg)
}

/// `argv[1..]` içinden genel seçenekleri korur, `update` alt komutunu atar.
pub fn restart_args(argv: &[String]) -> Vec<String> {
    let mut index = 0;
    while index < argv.len() {
        let token = argv[index].as_str();
        if matches!(token, "--workspace" | "-w" | "--model" | "-m") {
            index += 2;
        } else if token.starts_with("--workspace=")
            || token.starts_with("--model=")
            || ((token.starts_with("-w") || token.starts_with("-m")) && token.len() > 2)
        {
            index += 1;
        } else {
            if token == "update" {
                return argv[..index].to_vec();
            }
            break;
        }
    }
    argv.to_vec()
}

/// Süreci yeni ikiliyle yeniden başlatır (başarılıysa geri dönmez).
pub fn restart_process() -> String {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = restart_args(&argv);
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("evren"));
    log(&format!("RESTART: {} {}", exe.display(), args.join(" ")));
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(&exe).args(&args).exec();
        return err.to_string();
    }
    #[cfg(not(unix))]
    {
        match Command::new(&exe).args(&args).status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(0)),
            Err(e) => e.to_string(),
        }
    }
}

/// Güncelleme kaynağı satırı.
pub fn source_line() -> String {
    let kind = if is_git_checkout(&get_project_root()) {
        "git checkout"
    } else {
        "kaynak klonu"
    };
    format!("Güncelleme kaynağı: {kind} (github.com/{REPO_OWNER}/{REPO_NAME})")
}

pub const SPINNER_TEXT: &str = "En son sürüm çekiliyor ve kuruluyor...";

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn restart_args_drop_update() {
        assert_eq!(
            restart_args(&v(&["-w", "update", "update"])),
            v(&["-w", "update"])
        );
        assert_eq!(
            restart_args(&v(&["--model=x", "update", "--no-restart"])),
            v(&["--model=x"])
        );
        assert_eq!(restart_args(&v(&["chat"])), v(&["chat"]));
        assert_eq!(restart_args(&v(&["update"])), Vec::<String>::new());
    }

    #[test]
    fn project_root_is_workspace() {
        assert!(get_project_root().join("crates").is_dir());
    }

    #[test]
    fn ps_quoting() {
        assert_eq!(ps_quote("C:\\a'b"), "'C:\\a''b'");
    }

    #[test]
    fn dirty_count_ignores_untracked() {
        assert_eq!(count_dirty(""), 0);
        assert_eq!(count_dirty("?? plans/x.md\n"), 0);
        assert_eq!(count_dirty(" M src/a.rs\n?? b\nA  c.rs\n"), 2);
    }
}
