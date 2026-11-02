//! Süre sınırlı küçük `git` çağrıları.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `git <args>` çalıştırır; başarılıysa kırpılmış stdout, değilse `None`.
pub fn git_run(args: &[&str], cwd: &Path, timeout: Duration) -> Option<String> {
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
