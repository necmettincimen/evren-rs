//! Sistem istemi bileşenleri: yürütme bağlamı, git özeti, alan notları,
//! planlayıcı/alt görev metinleri (Python `agent.py` üst kısmı ve
//! `config.py` `execution_context_block`/`git_context_block`).

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use evren_core::config::Config;
use evren_core::tools::file_ops::resolve_path;

/// Python `SYSTEM_PROMPT` (birebir).
pub const SYSTEM_PROMPT: &str = include_str!("prompts/system_prompt.txt");

pub const PLANNER_INSTRUCTION: &str = "[EVREN PLANLAYICI] Bu turda yalnızca uygulama planı çıkar; hiçbir dosyayı değiştirme.
Keşfi az turda yap: ilk turda ilgili anahtar kelimeleri TEK find_files(query=\"a|b|c\") ile ara ve bağımsız
okumaları aynı turda birlikte çağır; ikinci turda yalnızca gereken aralıkları view_file(start_line,end_line) ile oku.
findstr/grep kullanma, dosyaları baştan sona okuma.
Kullanıcıya soru sorma: belirsiz noktayı \"Varsayım:\" satırıyla yaz.
Son yanıtın yalnızca plan olsun: en fazla 10 numaralı adım; her adımda hedef dosya ve satır aralığı/fonksiyon,
yapılacak değişiklik ve doğrulama komutu. Uygulayıcı keşfi tekrarlamasın diye gördüğün mevcut fonksiyon
adlarını ve satır numaralarını plana yaz. Uzun gerekçe yazma.";

/// Alt görev (delegate) istemi; `{task}` görev metniyle değiştirilir.
pub const DELEGATE_INSTRUCTION: &str =
    "[ALT GÖREV] Ana ajan sana bağımsız bir araştırma görevi devretti. Temiz \
bağlamsın: ana konuşmayı göremezsin; görev metni kendi başına yeterlidir. \
Yalnızca salt-okuma araçlarıyla (view_file, find_files, list_directory, \
salt-okuma run_command, web_fetch) çalış; yazma yapma ve kullanıcıya sorma. \
En fazla birkaç adımda kanıt topla (dosya:satır referanslarıyla) ve son \
yanıtını 3-8 cümlelik kısa bir özet olarak yaz; özet ana ajanın geçmişine \
girecek, ham çıktı değil sonuç isteniyor.\n\nGÖREV:\n{task}";

pub const CHUNKED_WRITE_NOTE: &str = "[SİSTEM] Son write_file/edit_file çağrısı çıktı token sınırında kesildi ve ÇALIŞMADI. \
Aynı çağrıyı tekrarlama. İçeriği en çok ~2500 karakterlik parçalara böl: ilk parçayı \
write_file ile, sonrakileri write_file(append=true) ile ekle; her parçadan önce düşünmeyi kısa tut. \
Dosya zaten kısmen yazıldıysa baştan yazma; view_file ile sonunu gör ve kaldığın yerden append et.";

/// Planı uygulayıcıya veren görev eki.
pub fn execution_brief(plan: &str, planner: &str) -> String {
    format!(
        "UYGULAMA PLANI (planlayıcı model: {planner}):\n{}\n\n\
         Bu planı sırayla uygula. Planı yeniden tartışma veya baştan analiz etme; yalnızca \
         bir adım canlı kodla çelişirse o adımı uyarla. Bağımsız adımları aynı turda çağır.",
        plan.trim()
    )
}

/// (not, tetikleyiciler): görev metninde tetikleyici geçerse not eklenir.
const DOMAIN_NOTES: &[(&str, &[&str])] = &[
    (
        include_str!("domain_notes/http_argo.md"),
        &[
            "argo",
            "curl",
            "/api/v1",
            "http://",
            "https://",
            "resource-tree",
        ],
    ),
    (include_str!("domain_notes/drone.md"), &["drone"]),
    (
        include_str!("domain_notes/k8s.md"),
        &[
            "kubectl",
            "k8s",
            "kubernetes",
            "pod",
            "namespace",
            "deployment",
        ],
    ),
];

/// Göreve uygun alan notları; yoksa boş dize.
pub fn domain_hints(text: &str) -> String {
    let lowered = text.to_lowercase();
    DOMAIN_NOTES
        .iter()
        .filter(|(_, triggers)| triggers.iter().any(|t| lowered.contains(t)))
        .map(|(note, _)| note.trim())
        .filter(|n| !n.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn platform_system() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "Darwin",
        "linux" => "Linux",
        other => other,
    }
}

/// Python `shell_label()`.
pub fn shell_label(config: &Config) -> String {
    if !cfg!(windows) {
        return "/bin/sh".to_string();
    }
    match config.windows_shell() {
        "bash" => {
            "Git Bash (POSIX: grep, sed, find, head, tail, wc, çok satırlı betik olur)".to_string()
        }
        "powershell" => "PowerShell".to_string(),
        _ => std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()),
    }
}

/// Yerel komut ortamını anlatan blok (istem önbelleği için saat yok).
pub fn execution_context_block(workspace_root: &Path, config: &Config) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d");
    let root = resolve_path(workspace_root);
    format!(
        "YEREL ÇALIŞTIRMA BAĞLAMI\n\
         Bugünün tarihi: {today}\n\
         İşletim sistemi: {}\n\
         Çalışma alanı ve run_command çalışma dizini: {}\n\
         run_command kabuğu: {}\n\
         run_command, EVREN CLI sürecinin çalıştığı makinede çalışır; SSH bağlantısı kurmaz.\n\
         Windows'ta PowerShell sözdizimi gerekiyorsa run_command'a use_powershell=true ver; \
         aksi halde komutu bu kabuğun sözdizimiyle yaz (Git Bash'te Unix yolları: /d/proje veya göreli yol).\n\
         CLI bir sunucunun terminalinde çalışıyorsa o sunucu yerel makinedir.\n\
         Son yanıt: istenmedikçe en fazla ~12 satır; 'kısa' veya madde sayısı verildiyse ona uy.",
        platform_system(),
        root.display(),
        shell_label(config)
    )
}

/// `git <args>` çıktısı (kırpılmış); git yoksa, hata verirse ya da süre aşılırsa `None`.
fn git_run(args: &[&str], cwd: &Path, timeout: Duration) -> Option<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Çıktı küçük olduğundan önce okunur, sonra süre sınırı ile beklenir.
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }
    let out = reader.join().ok()?;
    Some(String::from_utf8_lossy(&out).trim().to_string())
}

/// Oturum başında bir kez alınan git özeti; git yoksa boş dize.
pub fn git_context_block(workspace_root: &Path, timeout: Duration) -> String {
    if git_run(
        &["rev-parse", "--is-inside-work-tree"],
        workspace_root,
        timeout,
    )
    .as_deref()
        != Some("true")
    {
        return String::new();
    }
    let branch =
        git_run(&["branch", "--show-current"], workspace_root, timeout).unwrap_or_default();
    let mut default_branch = String::new();
    if let Some(head_ref) = git_run(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        workspace_root,
        timeout,
    ) {
        if !head_ref.is_empty() {
            default_branch = head_ref
                .split_once('/')
                .map(|(_, rest)| rest.to_string())
                .unwrap_or(head_ref);
        }
    }
    if default_branch.is_empty() {
        for candidate in ["main", "master"] {
            let refname = format!("refs/heads/{candidate}");
            if git_run(
                &["rev-parse", "--verify", &refname],
                workspace_root,
                timeout,
            )
            .is_some_and(|s| !s.is_empty())
            {
                default_branch = candidate.to_string();
                break;
            }
        }
    }
    let mut lines = vec!["GIT DURUMU (oturum başında alındı, sabittir)".to_string()];
    lines.push(format!(
        "Dal: {}",
        if branch.is_empty() {
            "(ayrık HEAD)"
        } else {
            &branch
        }
    ));
    if !default_branch.is_empty() {
        lines.push(format!("Ana dal tahmini: {default_branch}"));
    }
    if let Some(log) = git_run(
        &["log", "-3", "--pretty=format:%h %s"],
        workspace_root,
        timeout,
    ) {
        if !log.is_empty() {
            lines.push("Son commit'ler:".to_string());
            lines.extend(log.lines().map(|l| format!("  {l}")));
        }
    }
    if let Some(status) = git_run(&["status", "--porcelain"], workspace_root, timeout) {
        let entries: Vec<&str> = status.lines().filter(|l| !l.trim().is_empty()).collect();
        let untracked = entries.iter().filter(|l| l.starts_with("??")).count();
        lines.push(format!(
            "Değişen dosya: {} | İzlenmeyen dosya: {untracked}",
            entries.len() - untracked
        ));
    }
    lines.join("\n")
}

/// Verilen uzantıda dosya var mı (özyinelemeli, `.git` atlanır).
fn any_file_with_ext(dir: &Path, ext: &str, depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if depth > 0 && any_file_with_ext(&path, ext, depth - 1) {
                return true;
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            return true;
        }
    }
    false
}

/// Projenin test komutu tahmini; bulunamazsa boş dize.
pub fn detect_test_command(workspace_root: &Path) -> String {
    let root = workspace_root;
    if any_file_with_ext(root, "sln", 0) || any_file_with_ext(root, "csproj", 12) {
        return "dotnet test".to_string();
    }
    let pkg = root.join("package.json");
    if pkg.is_file() {
        if let Ok(text) = std::fs::read_to_string(&pkg) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if value.get("scripts").and_then(|s| s.get("test")).is_some() {
                    return "npm test".to_string();
                }
            }
        }
    }
    if root.join("tests").is_dir() && any_file_with_ext(root, "py", 12) {
        return "pytest".to_string();
    }
    String::new()
}

/// Kod değişti ama test koşulmadıysa modele hatırlatma; komut yoksa boş.
pub fn verification_note(workspace_root: &Path) -> String {
    let command = detect_test_command(workspace_root);
    if command.is_empty() {
        return String::new();
    }
    format!(
        "[EVREN] DOĞRULAMA: Kod dosyası değiştirdin ama bundan sonra test/derleme çalıştırmadın. \
         Bitirmeden önce `{command}` komutunu çalıştır; hata varsa düzelt, geçerse kısa özetle bitir."
    )
}

/// Sistem istemi: temel metin + yürütme bağlamı + (varsa) git bloğu.
/// Proje dokümanı, bellek, rol ve dil blokları bu sürümde yok.
pub fn compose_system(
    base: &str,
    workspace_root: &Path,
    config: &Config,
    git_block: &str,
) -> String {
    let mut blocks = vec![execution_context_block(workspace_root, config)];
    let roots_note = evren_core::tools::roots::prompt_note(workspace_root);
    if !roots_note.is_empty() {
        blocks.push(roots_note);
    }
    if !git_block.is_empty() {
        blocks.push(git_block.to_string());
    }
    let mem = evren_core::memory::memory_context_block(workspace_root, 10);
    if !mem.is_empty() {
        blocks.push(mem);
    }
    let role = role_context_block(workspace_root);
    if !role.is_empty() {
        blocks.push(role);
    }
    let lang = evren_core::language::language_context_block(workspace_root);
    if !lang.is_empty() {
        blocks.push(lang);
    }
    format!("{}\n\n{}", base.trim_end(), blocks.join("\n\n"))
}

/// `.evren/settings.json` içindeki `role` alanı (`/rol`); yoksa boş dize.
pub fn persistent_role(workspace_root: &Path) -> String {
    let path = evren_core::config::get_evren_dir(workspace_root).join("settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let text = text.trim_start_matches('\u{feff}');
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| {
            v.get("role")
                .and_then(|r| r.as_str())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

/// Kalıcı rol için sistem bloğu (`role_context_block`); ayarlı değilse boş.
pub fn role_context_block(workspace_root: &Path) -> String {
    let role = persistent_role(workspace_root);
    if role.is_empty() {
        String::new()
    } else {
        format!("KALICI ROL (kullanıcı ayarı, her görevde geçerli)\nRol: {role}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn hints_by_trigger() {
        assert!(!domain_hints("kubectl get pods").is_empty());
        assert_eq!(domain_hints("merhaba"), "");
    }

    #[test]
    fn brief_and_prompt() {
        let b = execution_brief("  1. x  ", "m");
        assert!(b.starts_with("UYGULAMA PLANI (planlayıcı model: m):\n1. x\n\n"));
        assert!(SYSTEM_PROMPT.len() > 100);
    }

    #[test]
    fn test_command_detection() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_test_command(dir.path()), "");
        let _ = std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"x"}}"#,
        );
        assert_eq!(detect_test_command(dir.path()), "npm test");
        let _ = std::fs::write(dir.path().join("a.sln"), "");
        assert_eq!(detect_test_command(dir.path()), "dotnet test");
    }
}
