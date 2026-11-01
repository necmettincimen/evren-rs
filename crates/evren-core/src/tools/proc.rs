//! Süreç yardımcıları (Python `proc_utils.py`): süreç ağacını öldürme,
//! zaman aşımlı çalıştırma ve canlı akış. Windows'ta her çocuk bir iş
//! nesnesine (job object) bağlanır; torunlar dahil hepsi birlikte ölür.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Zaman aşımında dönen çıkış kodu.
pub const TIMEOUT_RETURN_CODE: i32 = -1;

/// Çalıştırma sonucu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Hangi akıştan geldi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamTag {
    Stdout,
    Stderr,
}

impl StreamTag {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamTag::Stdout => "stdout",
            StreamTag::Stderr => "stderr",
        }
    }
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };

    /// Bir çocuk sürecin iş nesnesi tutamacı.
    pub struct JobHandle(HANDLE);

    // HANDLE ham işaretçidir; iş nesnesi tutamacı iş parçacıkları arasında taşınabilir.
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl JobHandle {
        /// Yeni başlatılmış süreci kendi iş nesnesine koyar.
        pub fn attach(child: &Child) -> Option<JobHandle> {
            // SAFETY: Win32 çağrıları geçerli tutamaçlarla yapılır; başarısızlıkta tutamaç kapatılır.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let proc_handle = child.as_raw_handle() as HANDLE;
                if AssignProcessToJobObject(job, proc_handle) == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(JobHandle(job))
            }
        }

        /// İş nesnesindeki tüm süreçleri sonlandırır.
        pub fn terminate(&self) {
            // SAFETY: tutamaç `attach` ile alınmış geçerli bir iş nesnesidir.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            // SAFETY: tutamaç yalnız burada kapatılır.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod job {
    use std::process::Child;

    /// POSIX'te iş nesnesi yoktur; süreç grubu kullanılır.
    pub struct JobHandle;

    impl JobHandle {
        pub fn attach(_child: &Child) -> Option<JobHandle> {
            None
        }
        pub fn terminate(&self) {}
    }
}

pub use job::JobHandle;

/// Kendi grubunda başlatılmış, iş nesnesine bağlı çocuk süreç.
pub struct TreeChild {
    pub child: Child,
    job: Option<JobHandle>,
}

impl TreeChild {
    /// Süreç ağacını öldürür (iş nesnesi + `taskkill /F /T` / `killpg`).
    pub fn kill_tree(&mut self) {
        if let Some(job) = self.job.take() {
            job.terminate();
        }
        kill_process_tree(self.child.id());
        let _ = self.child.kill();
    }

    /// Bitmiş sürecin iş nesnesini serbest bırakır (çocukları çalışmaya devam eder).
    pub fn release_job(&mut self) {
        self.job = None;
    }

    /// `wait(timeout=...)`: süre içinde biterse çıkış kodunu döner.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(exit_code(status)),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return None,
            }
        }
    }

    /// Süreç hâlâ çalışıyor mu?
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Bittiyse çıkış kodu.
    pub fn poll(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(exit_code(status)),
            _ => None,
        }
    }
}

impl Drop for TreeChild {
    fn drop(&mut self) {
        if self.running() {
            self.kill_tree();
        }
    }
}

/// `ExitStatus` → tam sayı (sinyalle ölen süreç için -1 yerine -sinyal).
pub fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(c) = status.code() {
        return c;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return -sig;
        }
    }
    -1
}

/// Çocuğu kendi süreç grubunda/oturumunda başlatır (`_popen_kwargs_for_group`).
pub fn spawn_in_group(cmd: &mut Command) -> std::io::Result<TreeChild> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    let job = JobHandle::attach(&child);
    Ok(TreeChild { child, job })
}

/// Süreci ve tüm torunlarını öldürür (platformlar arası).
pub fn kill_process_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-9", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn decode_line(bytes: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(bytes).into_owned();
    // Python metin modu (universal newlines): \r\n -> \n
    if s.ends_with("\r\n") {
        s.truncate(s.len() - 2);
        s.push('\n');
    }
    s
}

fn reader_thread<R: Read + Send + 'static>(
    stream: R,
    sink: mpsc::Sender<(StreamTag, Option<String>)>,
    tag: StreamTag,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if sink.send((tag, Some(decode_line(&buf)))).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = sink.send((tag, None));
    });
}

/// Komutu çalıştırır; zaman aşımında tüm süreç ağacı öldürülür.
/// Zaman aşımında `code=-1`, `timed_out=true`.
pub fn run_with_tree_kill(
    cmd: &mut Command,
    timeout: Option<Duration>,
) -> std::io::Result<RunOutcome> {
    stream_with_tree_kill(cmd, timeout, |_, _| {})
}

/// Komutu çalıştırır ve satırları üretildikçe `on_line` ile verir.
pub fn stream_with_tree_kill(
    cmd: &mut Command,
    timeout: Option<Duration>,
    mut on_line: impl FnMut(StreamTag, &str),
) -> std::io::Result<RunOutcome> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut proc = spawn_in_group(cmd)?;
    let (tx, rx) = mpsc::channel();
    let mut open_streams = 0;
    if let Some(out) = proc.child.stdout.take() {
        reader_thread(out, tx.clone(), StreamTag::Stdout);
        open_streams += 1;
    }
    if let Some(err) = proc.child.stderr.take() {
        reader_thread(err, tx.clone(), StreamTag::Stderr);
        open_streams += 1;
    }
    drop(tx);

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut timed_out = false;
    let deadline = timeout.map(|t| Instant::now() + t);

    while open_streams > 0 {
        let mut wait = Duration::from_millis(200);
        if let Some(d) = deadline {
            let now = Instant::now();
            if now >= d {
                timed_out = true;
                break;
            }
            wait = wait.min(d - now);
        }
        match rx.recv_timeout(wait) {
            Ok((_, None)) => open_streams -= 1,
            Ok((tag, Some(line))) => {
                match tag {
                    StreamTag::Stdout => stdout.push_str(&line),
                    StreamTag::Stderr => stderr.push_str(&line),
                }
                on_line(tag, &line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if proc.poll().is_some() && rx.try_recv().is_err() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if timed_out {
        proc.kill_tree();
    }
    let mut code = proc.wait_timeout(Duration::from_secs(5));
    if code.is_none() {
        proc.kill_tree();
        code = proc.wait_timeout(Duration::from_secs(5));
    }
    // Kalan tamponlanmış satırları boşalt.
    while let Ok((tag, line)) = rx.try_recv() {
        let Some(line) = line else { continue };
        match tag {
            StreamTag::Stdout => stdout.push_str(&line),
            StreamTag::Stderr => stderr.push_str(&line),
        }
        on_line(tag, &line);
    }
    if proc.running() {
        proc.kill_tree();
    } else {
        proc.release_job();
    }
    let code = if timed_out {
        TIMEOUT_RETURN_CODE
    } else {
        code.unwrap_or(TIMEOUT_RETURN_CODE)
    };
    Ok(RunOutcome {
        code,
        stdout,
        stderr,
        timed_out,
    })
}
