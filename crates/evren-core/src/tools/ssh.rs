//! Kayıtlı uzak host'larda komut ve dosya işlemleri (Python `ssh_ops.py`,
//! `remote_presets.py`). Sistem `ssh` istemcisi kullanılır; `BatchMode=yes` parola
//! istemine takılmayı önler, uzak yazma `allow_write` ve onay ister, her işlem
//! `.evren/logs/ssh_audit.log` dosyasına yazılır.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;

use super::command::{spill_long_output, truncate_output};
use super::proc::{run_with_tree_kill, spawn_in_group, StreamTag};
use super::ToolContext;
use crate::ssh_hosts::{get_host, load_hosts, unknown_alias_message, SshHost};

mod presets_data {
    include!("ssh_presets_data.rs");
}

pub const DEFAULT_SSH_TIMEOUT: i64 = 60;
pub const MAX_SSH_TIMEOUT: i64 = 600;
const READ_MAX_CHARS: usize = 150_000;

const CONNECTION_ERRORS: [&str; 7] = [
    "timed out",
    "connection refused",
    "no route to host",
    "could not resolve",
    "network is unreachable",
    "permission denied",
    "host key verification failed",
];

/// `host` üzerinde `remote_command` çalıştıran `ssh` argv'si.
pub fn build_ssh_command(host: &SshHost, remote_command: &str) -> Vec<String> {
    let mut argv: Vec<String> = [
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ConnectTimeout=15",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if host.port != 0 && host.port != 22 {
        argv.extend(["-p".to_string(), host.port.to_string()]);
    }
    if !host.identity_file.is_empty() {
        argv.extend(["-i".to_string(), host.identity_file.clone()]);
    }
    argv.push(host.target());
    argv.push(remote_command.to_string());
    argv
}

fn command_of(argv: &[String], root: &Path) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(root);
    cmd
}

/// Denetim günlüğüne satır ekler (en iyi çaba; hata yutulur).
pub fn audit_log(root: &Path, alias: &str, action: &str, detail: &str, code: Option<i32>) {
    let dir = root.join(".evren").join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let code_str = code.map(|c| format!(" exit={c}")).unwrap_or_default();
    let detail = crate::redact::redact_text(detail);
    let line = format!("[{ts}] host={alias} action={action}{code_str} :: {detail}\n");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("ssh_audit.log"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn sudo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(^|[;&|(]\s*|\s)sudo\s+").unwrap_or_else(|_| super::command::never_re())
    })
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Terminal yok: `sudo` parola beklemek yerine hemen düşsün (`sudo -n`).
pub fn force_sudo_noninteractive(command: &str) -> String {
    let mut out = String::with_capacity(command.len() + 8);
    let mut last = 0;
    for caps in sudo_re().captures_iter(command) {
        let (Some(m), Some(prefix)) = (caps.get(0), caps.get(1)) else {
            continue;
        };
        let rest = &command[m.end()..];
        let already = ["-n", "--non-interactive"].iter().any(|flag| {
            rest.strip_prefix(flag)
                .is_some_and(|after| !after.chars().next().is_some_and(is_word_char))
        });
        if already {
            continue;
        }
        out.push_str(&command[last..m.start()]);
        out.push_str(prefix.as_str());
        out.push_str("sudo -n ");
        last = m.end();
    }
    out.push_str(&command[last..]);
    out
}

fn is_connection_error(err: &str) -> bool {
    let lowered = err.to_lowercase();
    CONNECTION_ERRORS.iter().any(|m| lowered.contains(m))
}

fn resolve(alias: &str, root: &Path) -> Result<SshHost, String> {
    match get_host(alias, Some(root)) {
        Ok(Some(h)) => Ok(h),
        Ok(None) => Err(unknown_alias_message(alias, Some(root))),
        Err(e) => Err(format!("HATA: {e}")),
    }
}

/// Uzak komut çalıştırır; sonuç `tool_run_command` biçimini izler.
pub fn ssh_run(
    ctx: &ToolContext,
    alias: &str,
    command: &str,
    timeout_seconds: Option<i64>,
) -> String {
    let root = ctx.root();
    let host = match resolve(alias, &root) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let timeout = timeout_seconds
        .filter(|t| *t != 0)
        .unwrap_or(DEFAULT_SSH_TIMEOUT)
        .clamp(1, MAX_SSH_TIMEOUT);
    let command = force_sudo_noninteractive(command);
    let argv = build_ssh_command(&host, &command);
    let outcome = match run_with_tree_kill(
        &mut command_of(&argv, &root),
        Some(Duration::from_secs(timeout as u64)),
    ) {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return "HATA: 'ssh' istemcisi bulunamadı. Windows'ta OpenSSH Client özelliğini kurun."
                .to_string()
        }
        Err(e) => return format!("HATA: SSH komutu çalıştırılamadı: {e}"),
    };
    if outcome.timed_out {
        audit_log(
            &root,
            alias,
            "run",
            &format!("TIMEOUT: {command}"),
            Some(-1),
        );
        return format!(
            "HATA: SSH komutu zaman aşımına uğradı ({timeout} saniye). Süreç ağacı sonlandırıldı."
        );
    }
    audit_log(&root, alias, "run", &command, Some(outcome.code));
    let out = outcome.stdout.trim();
    let err = outcome.stderr.trim();
    let mut lines = vec![format!("[{alias}] Çıkış Kodu: {}", outcome.code)];
    if !out.is_empty() {
        let spilled = spill_long_output(out, Some(&root), &format!("ssh-{alias}"), &command);
        if spilled != out {
            // Canlı akıtılmadı: tam çıktı kullanıcının terminaline basılır.
            for line in crate::output_summary::splitlines(out) {
                ctx.emit_output(StreamTag::Stdout, &format!("{line}\n"));
            }
        }
        lines.push(format!("STDOUT:\n{spilled}"));
    }
    if !err.is_empty() {
        lines.push(format!("STDERR:\n{}", truncate_output(err)));
    }
    if outcome.code == 255 && is_connection_error(err) {
        let port = if host.port == 0 { 22 } else { host.port };
        lines.push(format!(
            "[EVREN] Bağlantı kurulamadı: {}:{port} (alias={alias}). Uzak komut hiç çalışmadı. \
             Aynı çağrıyı tekrarlama; kullanıcıya ağ/VPN erişimini veya host kaydını sor.",
            host.target()
        ));
    }
    lines.join("\n")
}

fn quote_path(path: &str) -> String {
    path.replace('\'', "'\\''")
}

/// Uzak dosyayı `cat` ile okur (salt okuma).
pub fn ssh_read_file(root: &Path, alias: &str, remote_path: &str) -> String {
    let host = match resolve(alias, root) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let argv = build_ssh_command(&host, &format!("cat '{}'", quote_path(remote_path)));
    let outcome = match run_with_tree_kill(
        &mut command_of(&argv, root),
        Some(Duration::from_secs(DEFAULT_SSH_TIMEOUT as u64)),
    ) {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return "HATA: 'ssh' istemcisi bulunamadı.".to_string()
        }
        Err(e) => return format!("HATA: Uzak dosya okunamadı: {e}"),
    };
    if outcome.timed_out {
        audit_log(
            root,
            alias,
            "read",
            &format!("TIMEOUT: {remote_path}"),
            Some(-1),
        );
        return format!("HATA: Uzak dosya okuma zaman aşımına uğradı ({remote_path}).");
    }
    audit_log(root, alias, "read", remote_path, Some(outcome.code));
    if outcome.code != 0 {
        return format!(
            "HATA: Uzak dosya okunamadı ({remote_path}): {}",
            outcome.stderr.trim()
        );
    }
    let mut content = outcome.stdout;
    if let Some((idx, _)) = content.char_indices().nth(READ_MAX_CHARS) {
        content.truncate(idx);
        content.push_str(&format!(
            "\n\n... [Uzak dosya {READ_MAX_CHARS} karakterde kesildi]"
        ));
    }
    format!("--- {alias}:{remote_path} ---\n{content}")
}

/// Uzak dosyayı stdin üzerinden yazar (`cat > path`); `allow_write` gerekir.
pub fn ssh_write_file(root: &Path, alias: &str, remote_path: &str, content: &str) -> String {
    let host = match resolve(alias, root) {
        Ok(h) => h,
        Err(e) => return e,
    };
    if !host.allow_write {
        return format!(
            "HATA: '{alias}' host'unda uzak yazma kapalı (allow_write=false). \
             Bilinçli olarak açmak için ssh_hosts.json'da allow_write=true yapın."
        );
    }
    let argv = build_ssh_command(&host, &format!("cat > '{}'", quote_path(remote_path)));
    let mut cmd = command_of(&argv, root);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut proc = match spawn_in_group(&mut cmd) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return "HATA: 'ssh' istemcisi bulunamadı.".to_string()
        }
        Err(e) => return format!("HATA: Uzak dosya yazılamadı: {e}"),
    };
    if let Some(mut stdin) = proc.child.stdin.take() {
        let data = content.as_bytes().to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let stderr_reader = proc.child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    });
    if let Some(mut out) = proc.child.stdout.take() {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut out, &mut std::io::sink());
        });
    }
    let Some(code) = proc.wait_timeout(Duration::from_secs(DEFAULT_SSH_TIMEOUT as u64)) else {
        proc.kill_tree();
        audit_log(
            root,
            alias,
            "write",
            &format!("TIMEOUT: {remote_path}"),
            Some(-1),
        );
        return format!("HATA: Uzak dosya yazma zaman aşımına uğradı ({remote_path}).");
    };
    audit_log(root, alias, "write", remote_path, Some(code));
    if code != 0 {
        let err = stderr_reader
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        return format!(
            "HATA: Uzak dosya yazılamadı ({remote_path}): {}",
            err.trim()
        );
    }
    format!(
        "BAŞARILI: '{alias}:{remote_path}' uzak dosyası yazıldı ({} karakter).",
        content.chars().count()
    )
}

/// Kayıtlı host listesi.
pub fn ssh_list_hosts(root: &Path) -> String {
    let hosts = match load_hosts(Some(root)) {
        Ok(h) => h,
        Err(e) => return format!("HATA: {e}"),
    };
    if hosts.is_empty() {
        return "Kayıtlı SSH host'u yok. Eklemek için: /ssh add <alias> <user@host> [port]"
            .to_string();
    }
    let mut sorted: Vec<&SshHost> = hosts.iter().collect();
    sorted.sort_by(|a, b| a.alias.cmp(&b.alias));
    let lines: Vec<String> = sorted
        .into_iter()
        .map(|h| {
            let alias = &h.alias;
            let flag = if h.allow_write {
                "yazma-açık"
            } else {
                "salt-okuma"
            };
            let desc = if h.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", h.description)
            };
            format!("  • {alias:<16} {}:{} [{flag}]{desc}", h.target(), h.port)
        })
        .collect();
    format!("Kayıtlı SSH Host'ları:\n{}", lines.join("\n"))
}

pub fn preset_names() -> Vec<&'static str> {
    let mut names: Vec<&str> = presets_data::PRESETS.iter().map(|p| p.0).collect();
    names.sort();
    names
}

/// Hazır uzak komutlar (salt okuma): (ad, açıklama).
pub fn presets() -> impl Iterator<Item = (&'static str, &'static str)> {
    presets_data::PRESETS.iter().map(|p| (p.0, p.1))
}

fn valid_namespace(ns: &str) -> bool {
    let b = ns.as_bytes();
    let ok = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty()
        && b.len() <= 63
        && ok(&b[0])
        && ok(&b[b.len() - 1])
        && b.iter().all(|c| ok(c) || *c == b'-')
}

/// Preset'in kabuk komutu (Python `build_preset`).
pub fn build_preset(name: &str, namespace: Option<&str>) -> Result<String, String> {
    let Some(&(_, _, tpl)) = presets_data::PRESETS.iter().find(|p| p.0 == name) else {
        return Err(format!(
            "Bilinmeyen preset: '{name}'. Geçerli: {}.",
            preset_names().join(", ")
        ));
    };
    let ns = namespace.unwrap_or("").trim();
    if !ns.is_empty() && !valid_namespace(ns) {
        return Err(format!("Geçersiz namespace: '{ns}'."));
    }
    // `-n ns` çıktısında NAMESPACE sütunu yok; awk alan numaraları bir kayar.
    let (scope, ready, status, restarts) = if ns.is_empty() {
        ("-A".to_string(), "3", "4", "5")
    } else {
        (format!("-n {ns}"), "2", "3", "4")
    };
    let body = tpl
        .replace("@@SCOPE@@", &scope)
        .replace("@@READY@@", ready)
        .replace("@@STATUS@@", status)
        .replace("@@RESTARTS@@", restarts);
    Ok(format!("{}{body}", presets_data::API_GUARD))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn digest(s: &str) -> String {
        let h = Sha256::digest(s.as_bytes());
        h.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn presets_match_python() {
        let cases = [
            ("k8s_health", None, 1320, "1f275aea4919a8c8"),
            ("k8s_health", Some("kube-system"), 1392, "558a49eb11c3b499"),
            ("k8s_resources", None, 766, "60a22b46d4a8111b"),
            (
                "k8s_resources",
                Some("kube-system"),
                778,
                "41ca5daff070c948",
            ),
        ];
        for (name, ns, len, hash) in cases {
            let cmd = build_preset(name, ns).unwrap();
            assert_eq!(cmd.chars().count(), len, "{name} {ns:?}");
            assert_eq!(digest(&cmd), hash, "{name} {ns:?}");
        }
        assert!(build_preset("x", None)
            .unwrap_err()
            .starts_with("Bilinmeyen preset: 'x'."));
        assert_eq!(
            build_preset("k8s_health", Some("Bad_NS")).unwrap_err(),
            "Geçersiz namespace: 'Bad_NS'."
        );
    }

    #[test]
    fn sudo_gets_non_interactive() {
        assert_eq!(force_sudo_noninteractive("sudo ls"), "sudo -n ls");
        assert_eq!(
            force_sudo_noninteractive("cd /x && sudo  systemctl restart a"),
            "cd /x && sudo -n systemctl restart a"
        );
        assert_eq!(force_sudo_noninteractive("sudo -n ls"), "sudo -n ls");
        assert_eq!(
            force_sudo_noninteractive("sudo --non-interactive ls; sudo id"),
            "sudo --non-interactive ls; sudo -n id"
        );
        assert_eq!(force_sudo_noninteractive("pseudo ls"), "pseudo ls");
    }

    #[test]
    fn argv_and_listing() {
        let mut h = SshHost::new("web", "10.0.0.5");
        h.user = "root".into();
        h.port = 2222;
        h.identity_file = "~/.ssh/k".into();
        assert_eq!(
            build_ssh_command(&h, "uptime"),
            vec![
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ConnectTimeout=15",
                "-p",
                "2222",
                "-i",
                "~/.ssh/k",
                "root@10.0.0.5",
                "uptime"
            ]
        );
        assert!(is_connection_error(
            "ssh: connect to host x port 22: Connection refused"
        ));
        assert_eq!(quote_path("a'b"), "a'\\''b");
    }
}
