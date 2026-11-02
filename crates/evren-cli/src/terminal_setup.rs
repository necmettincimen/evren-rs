//! `evren setup-terminal`: EVREN'i Windows Terminal içinde Git Bash ile açar
//! (Python `terminal_setup.py`). Profil ayrı bir *fragment* dosyası olarak
//! eklenir; kullanıcının settings.json dosyasına dokunulmaz.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use evren_core::config::Config;
use serde_json::{json, Value};

use crate::plan::which;

pub const PROFILE_NAME: &str = "EVREN (Git Bash)";
pub const PROFILE_GUID: &str = "{6f3c8a52-4e7b-5d0e-9a1c-0e5e7c1e2b30}";

fn env_or(name: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(fallback)
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

pub fn fragment_path() -> PathBuf {
    env_or("LOCALAPPDATA", || home().join("AppData").join("Local"))
        .join("Microsoft")
        .join("Windows Terminal")
        .join("Fragments")
        .join("EVREN")
        .join("evren.json")
}

pub fn shortcut_path() -> PathBuf {
    env_or("USERPROFILE", home)
        .join("Desktop")
        .join("EVREN.lnk")
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `...\Git\bin\bash.exe` → `...\Git` altındaki Git simgesi.
fn git_icon(bash: &Path) -> Option<String> {
    let bash = evren_core::tools::file_ops::resolve_path(bash);
    let root = bash.parent()?.parent()?;
    [
        root.join("mingw64")
            .join("share")
            .join("git")
            .join("git-for-windows.ico"),
        root.join("git-bash.exe"),
    ]
    .into_iter()
    .find(|c| c.exists())
    .map(|c| text(&c))
}

pub fn build_profile(bash: &Path, start_dir: &str) -> Value {
    let mut profile = json!({
        "guid": PROFILE_GUID,
        "name": PROFILE_NAME,
        // Login kabuğu; `evren` çıkınca sekme kapanmaz, bash'te kalır.
        "commandline": format!("\"{}\" --login -i -c \"evren; exec bash -i\"", text(bash)),
        "startingDirectory": start_dir,
        "hidden": false,
    });
    if let (Some(icon), Some(map)) = (git_icon(bash), profile.as_object_mut()) {
        map.insert("icon".into(), Value::String(icon));
    }
    profile
}

/// Fragment dosyasını (ve isteğe bağlı masaüstü kısayolunu) yazar; rapor satırları.
pub fn setup(config: &Config, start_dir: Option<&str>, shortcut: bool) -> Vec<String> {
    if !cfg!(windows) {
        return vec![
            "Bu komut yalnızca Windows içindir; diğer sistemlerde terminalinizde `evren` çalıştırın."
                .into(),
        ];
    }
    let Some(bash) = config.git_bash_path() else {
        return vec![
            "HATA: Git Bash bulunamadı. Git for Windows'u kurun ya da EVREN_GIT_BASH ile yolu verin."
                .into(),
        ];
    };
    let start = match start_dir {
        Some(d) if !d.is_empty() => text(&evren_core::tools::file_ops::resolve_path(Path::new(d))),
        _ => "%USERPROFILE%".to_string(),
    };
    let path = fragment_path();
    let doc = json!({ "profiles": [build_profile(&bash, &start)] });
    let body = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, body));
    if let Err(e) = written {
        return vec![format!("HATA: {}: {e}", text(&path))];
    }
    let mut lines = vec![
        format!(
            "Windows Terminal profili eklendi: '{PROFILE_NAME}' ({})",
            text(&path)
        ),
        format!("Kabuk: {} | Başlangıç klasörü: {start}", text(&bash)),
        "Windows Terminal'i yeniden açın; profil açılır menüde görünür.".into(),
    ];
    if which("wt").is_none() {
        lines.push(
            "Uyarı: Windows Terminal (wt.exe) bulunamadı; Microsoft Store'dan kurabilirsiniz."
                .into(),
        );
    }
    if shortcut {
        lines.push(create_shortcut(&bash));
    }
    if which("evren").is_none() {
        lines.push(
            "Uyarı: 'evren' PATH'te değil; profil açılınca komut bulunamayabilir (pip install -e .)."
                .into(),
        );
    }
    lines
}

/// PowerShell + WScript.Shell ile `EVREN.lnk` oluşturur (30 sn sınır).
fn create_shortcut(bash: &Path) -> String {
    let link = shortcut_path();
    let (target, arguments) = match which("wt") {
        Some(wt) => (text(&wt), format!("-p \"{PROFILE_NAME}\"")),
        None => (
            text(bash),
            "--login -i -c \"evren; exec bash -i\"".to_string(),
        ),
    };
    let script = "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:EVREN_LNK);\
                  $s.TargetPath=$env:EVREN_TARGET;$s.Arguments=$env:EVREN_ARGS;$s.Save()";
    let spawned = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("EVREN_LNK", &link)
        .env("EVREN_TARGET", &target)
        .env("EVREN_ARGS", &arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let failure = match spawned {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "FileNotFoundError",
        Err(_) => "OSError",
        Ok(mut child) => {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                match child.try_wait() {
                    Ok(Some(st)) if st.success() => {
                        return format!("Masaüstü kısayolu oluşturuldu: {}", text(&link));
                    }
                    Ok(Some(_)) => break "CalledProcessError",
                    Ok(None) if Instant::now() >= deadline => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break "TimeoutExpired";
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(_) => break "OSError",
                }
            }
        }
    };
    format!("Uyarı: kısayol oluşturulamadı ({failure}).")
}

/// Fragment ve kısayolu siler; rapor satırları.
pub fn remove() -> Vec<String> {
    let mut out = Vec::new();
    for p in [fragment_path(), shortcut_path()] {
        if p.exists() && std::fs::remove_file(&p).is_ok() {
            out.push(format!("Silindi: {}", text(&p)));
        }
    }
    if let Some(dir) = fragment_path().parent() {
        let _ = std::fs::remove_dir(dir);
    }
    if out.is_empty() {
        out.push("Kaldırılacak EVREN terminal profili yok.".into());
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn profile_has_python_fields_in_order() {
        let p = build_profile(Path::new("C:/yok/Git/bin/bash.exe"), "%USERPROFILE%");
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["guid", "name", "commandline", "startingDirectory", "hidden"]
        );
        assert_eq!(p["name"], PROFILE_NAME);
        assert!(p["commandline"]
            .as_str()
            .unwrap()
            .ends_with("\" --login -i -c \"evren; exec bash -i\""));
    }

    #[test]
    fn fragment_lives_under_windows_terminal_fragments() {
        let p = fragment_path();
        assert!(p.ends_with(Path::new("Windows Terminal/Fragments/EVREN/evren.json")));
        assert!(shortcut_path().ends_with(Path::new("Desktop/EVREN.lnk")));
    }
}
