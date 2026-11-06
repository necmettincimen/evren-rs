//! Arka plan komutları (Python `background.py`): uzun süren süreçler
//! kendi süreç grubunda başlatılır, çıktısı sınırlı bir tamponda tutulur.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use super::proc::{kill_process_tree, spawn_in_group, TreeChild};

/// Bellekte tutulan en fazla çıktı karakteri.
pub const MAX_BUFFER_CHARS: usize = 200_000;
/// Tek `check` çağrısında dönen en fazla karakter.
pub const MAX_CHECK_CHARS: usize = 8_000;

const TRIM_MARKER: &str = "[eski çıktı kırpıldı]\n";

/// Süreç tamponu ve durumu (kilit altındaki paylaşılan kısım).
#[derive(Default)]
struct Shared {
    buffer: Vec<String>,
    chars: usize,
    read_offset: usize,
    exit_code: Option<i32>,
}

impl Shared {
    fn append(&mut self, text: String) {
        let n = text.chars().count();
        self.buffer.push(text);
        self.chars += n;
        if self.chars <= MAX_BUFFER_CHARS {
            return;
        }
        let mut keep: Vec<String> = Vec::new();
        let mut kept = 0usize;
        while let Some(chunk) = self.buffer.pop() {
            kept += chunk.chars().count();
            keep.push(chunk);
            if kept >= MAX_BUFFER_CHARS / 2 {
                break;
            }
        }
        keep.reverse();
        let dropped = self.chars.saturating_sub(kept);
        let mut buffer = Vec::with_capacity(keep.len() + 1);
        buffer.push(TRIM_MARKER.to_string());
        buffer.extend(keep);
        self.buffer = buffer;
        self.chars = kept + TRIM_MARKER.chars().count();
        self.read_offset = self.read_offset.saturating_sub(dropped);
    }

    fn new_output(&mut self) -> String {
        let text: String = self.buffer.concat();
        let chunk: String = text.chars().skip(self.read_offset).collect();
        self.read_offset = text.chars().count();
        chunk
    }
}

/// Tek bir arka plan süreci.
pub struct BackgroundProcess {
    pub id: String,
    pub command: String,
    pub started_at: Instant,
    pub stopped: bool,
    child: Arc<Mutex<TreeChild>>,
    shared: Arc<Mutex<Shared>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl BackgroundProcess {
    /// Süreç hâlâ çalışıyor mu?
    pub fn running(&self) -> bool {
        lock(&self.child).running()
    }

    /// Bittiyse çıkış kodu.
    pub fn exit_code(&self) -> Option<i32> {
        if let Some(c) = lock(&self.shared).exit_code {
            return Some(c);
        }
        lock(&self.child).poll()
    }

    /// Süreç kimliği.
    pub fn pid(&self) -> u32 {
        lock(&self.child).child.id()
    }

    /// Son okumadan bu yana biriken çıktı.
    pub fn new_output(&self) -> String {
        lock(&self.shared).new_output()
    }

    /// `çalışıyor` / `bitti (çıkış kodu: N)` [+ ` [durduruldu]`].
    pub fn status_line(&self) -> String {
        let mut s = if self.running() {
            "çalışıyor".to_string()
        } else {
            match self.exit_code() {
                Some(c) => format!("bitti (çıkış kodu: {c})"),
                None => "bitti (çıkış kodu: None)".to_string(),
            }
        };
        if self.stopped {
            s.push_str(" [durduruldu]");
        }
        s
    }

    /// Başlangıçtan bu yana geçen süre.
    pub fn elapsed_seconds(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64()
    }

    /// Süreç ağacını öldürüp çıkış kodunu bekler.
    fn stop_now(&mut self) {
        self.stopped = true;
        let pid = self.pid();
        {
            let mut child = lock(&self.child);
            child.kill_tree();
        }
        kill_process_tree(pid);
        let code = lock(&self.child).wait_timeout(Duration::from_secs(5));
        if let Some(code) = code {
            lock(&self.shared).exit_code = Some(code);
        }
    }
}

/// Arka plan süreç yöneticisi (`BackgroundManager`).
#[derive(Default)]
pub struct BackgroundManager {
    processes: BTreeMap<String, BackgroundProcess>,
    counter: u32,
}

/// `check` sonucu: (yeni çıktı, durum metni, çıkış kodu).
pub type CheckResult = (String, String, Option<i32>);

impl BackgroundManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Komutu başlatır ve `bg-N` kimliği döner.
    /// `argv` verilirse doğrudan, yoksa `command` kabuk üzerinden çalışır.
    pub fn start(
        &mut self,
        command: &str,
        cwd: &std::path::Path,
        argv: Option<&[String]>,
        env: &[(String, String)],
    ) -> std::io::Result<&BackgroundProcess> {
        self.counter += 1;
        let id = format!("bg-{}", self.counter);
        let mut cmd = match argv {
            Some(argv) if !argv.is_empty() => {
                let mut c = Command::new(&argv[0]);
                c.args(&argv[1..]);
                c
            }
            _ => shell_command(command),
        };
        cmd.current_dir(cwd).envs(env.iter().map(|(k, v)| (k, v)));
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut tree = spawn_in_group(&mut cmd)?;
        let stdout = tree.child.stdout.take();
        let stderr = tree.child.stderr.take();
        let child = Arc::new(Mutex::new(tree));
        let shared = Arc::new(Mutex::new(Shared::default()));

        // stdout ve stderr aynı tampona akar (Python: stderr=STDOUT).
        let mut readers = Vec::new();
        if let Some(out) = stdout {
            readers.push(drain_thread(out, Arc::clone(&shared)));
        }
        if let Some(err) = stderr {
            readers.push(drain_thread(err, Arc::clone(&shared)));
        }
        {
            let child = Arc::clone(&child);
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                for r in readers {
                    let _ = r.join();
                }
                let code = lock(&child).wait_timeout(Duration::from_secs(10));
                if let Some(code) = code {
                    lock(&shared).exit_code = Some(code);
                }
            });
        }

        let proc = BackgroundProcess {
            id: id.clone(),
            command: command.to_string(),
            started_at: Instant::now(),
            stopped: false,
            child,
            shared,
        };
        Ok(self.processes.entry(id).or_insert(proc))
    }

    /// Kimlikle süreç.
    pub fn get(&self, id: &str) -> Option<&BackgroundProcess> {
        self.processes.get(id.trim())
    }

    /// Yeni çıktı + durum; bilinmeyen kimlikte `None`.
    pub fn check(&self, id: &str) -> Option<CheckResult> {
        let proc = self.get(id)?;
        let output = proc.new_output();
        Some((output, proc.status_line(), proc.exit_code()))
    }

    /// Süreç ağacını sonlandırır; bilinmeyen kimlikte `false`.
    pub fn stop(&mut self, id: &str) -> bool {
        let Some(proc) = self.processes.get_mut(id.trim()) else {
            return false;
        };
        if proc.running() {
            proc.stop_now();
        } else if let Some(code) = lock(&proc.child).poll() {
            lock(&proc.shared).exit_code = Some(code);
        }
        true
    }

    /// Kayıtlı süreçler (kimlik sırasıyla).
    pub fn list(&self) -> Vec<&BackgroundProcess> {
        self.processes.values().collect()
    }

    /// Kayıtlı kimlikler.
    pub fn ids(&self) -> Vec<&str> {
        self.processes.keys().map(String::as_str).collect()
    }

    /// Çalışan tüm süreçleri öldürür; öldürülen sayısını döner.
    pub fn cleanup_all(&mut self) -> usize {
        let mut killed = 0;
        for proc in self.processes.values_mut() {
            if proc.running() {
                proc.stop_now();
                killed += 1;
            }
        }
        killed
    }
}

fn drain_thread<R: std::io::Read + Send + 'static>(
    stream: R,
    shared: Arc<Mutex<Shared>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let mut line = String::from_utf8_lossy(&buf).into_owned();
                    if line.ends_with("\r\n") {
                        line.truncate(line.len() - 2);
                        line.push('\n');
                    }
                    lock(&shared).append(line);
                }
            }
        }
    })
}

/// Platform kabuğu üzerinden komut (`shell=True`).
pub fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd.exe");
        c.args(["/d", "/c"]).raw_arg(command);
        c
    }
    #[cfg(not(windows))]
    {
        // Ajanlar bash sözdizimi yazar (`[[`, `<(..)`, diziler); /bin/sh dash olabilir.
        let sh = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
        let mut c = Command::new(sh);
        c.args(["-c", command]);
        c
    }
}
