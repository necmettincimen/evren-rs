//! `evren plan` (Python `plan_runner.py`): LLM'siz, deterministik plan
//! yürütücü. Bir JSON plan dosyasındaki adımları sırayla uygular; kural
//! tabanlı (find/replace, klonla, komut) işler için model çağrısı yapmaz.
//!
//! ```text
//! {"steps": [
//!   {"op": "clone_tree", "src": "a", "dst": "b", "replacements": {"Eski": "Yeni"}, "exclude": ["bin"]},
//!   {"op": "replace", "glob": "b/**/*.cs", "find": "x", "with": "y", "regex": false, "flags": "", "required": true},
//!   {"op": "write", "path": "b/x.txt", "content": "..."},
//!   {"op": "run", "cmd": "dotnet build", "cwd": "b", "timeout": 900, "ok_exit": [0], "shell": "bash",
//!    "save": "reports/ozet.txt", "min_count": 1, "count_regex": "bulunan=(\\d+)", "count_warn": false},
//!   {"op": "commit", "repo": "b", "message": "..."},
//!   {"op": "push", "repo": "b", "branch": "test", "target": "main", "base": "main", "allow_foreign": false},
//!   {"op": "agent", "prompt": "...", "repo": "b", "max_steps": 80, "timeout": 1800,
//!    "kind": "code" | "report" | "docs" | "explore", "inputs": ["reports/ozet.txt"],
//!    "skills": ["clickup"]},
//!   {"op": "live_diff", "file": "b/argo/wf.yaml", "live_cmd": "kubectl get ... -o yaml" | "live_ssh": {...}},
//!   {"op": "parallel", "max_workers": 3, "steps": [...]},
//!   {"op": "ssh" | "scp" | "http" | "wait", ...}
//! ]}
//! ```
//!
//! Hata olursa durur ve `--from N` ile devam komutunu yazar. Yollar çalışma
//! alanı köküne göredir ve kök dışına çıkamaz. Commit mesajında AI imzası
//! reddedilir. `push` yalnız `--allow-push` ile çalışır.

mod agent;
mod live_diff;
mod remote;

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use evren_core::tools::clone_tree::tool_clone_tree;
use evren_core::tools::file_ops::resolve_path;
use evren_core::tools::fs_tools::fnmatch;
use evren_core::tools::proc::spawn_in_group;
use evren_core::tools::ToolContext;
use regex::Regex;
use serde_json::Value;

use agent::{AgentRunner, ProcessRunner, Runtime};
use remote::{expand, ok_exit, py_repr, run_remote, REMOTE_OPS};

/// Plan adımı (JSON nesnesi).
pub type Step = serde_json::Map<String, Value>;
/// `save_as` ile kaydedilen plan değişkenleri.
pub type Ctx = HashMap<String, String>;

static PRINT_LOCK: Mutex<()> = Mutex::new(());
static PUSH_LOCK: Mutex<()> = Mutex::new(());

fn lock(m: &Mutex<()>) -> MutexGuard<'_, ()> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Satırı stdout'a yazar (eşzamanlı adımlarda satırlar karışmasın).
fn say(line: &str) {
    let _g = lock(&PRINT_LOCK);
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Python doğruluk değeri.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `str(value)` (f-string içindeki karşılığı).
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `bool(step.get(key, default))`.
fn flag(step: &Step, key: &str, default: bool) -> bool {
    step.get(key).map(py_truthy).unwrap_or(default)
}

/// `int(step.get(key, default))`.
fn int_or(step: &Step, key: &str, default: i64) -> i64 {
    match step.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        Some(Value::Bool(b)) => i64::from(*b),
        _ => default,
    }
}

/// `step.get(key)`; eksik ya da null ise `None`.
fn str_of(step: &Step, key: &str) -> Option<String> {
    match step.get(key) {
        None | Some(Value::Null) => None,
        Some(v) => Some(py_str(v)),
    }
}

/// `step[key]`; eksikse Python KeyError metni (`'key'`).
fn req_str(step: &Step, key: &str) -> Result<String, String> {
    match step.get(key) {
        None => Err(format!("'{key}'")),
        Some(v) => Ok(py_str(v)),
    }
}

/// İlk `n` karakter.
fn first_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Son `n` karakter.
fn last_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    match s.char_indices().nth(count - n) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

/// `"\n".join(text.strip().splitlines()[-n:])`.
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn norm_parts(p: &Path) -> Vec<String> {
    p.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| {
            let s = c.as_os_str().to_string_lossy().into_owned();
            if cfg!(windows) {
                s.to_lowercase()
            } else {
                s
            }
        })
        .collect()
}

/// Python `_p`: köke göre çözülen yol; kök dışına çıkarsa hata.
fn resolve_in_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let path = resolve_path(&root.join(rel));
    let (r, p) = (norm_parts(root), norm_parts(&path));
    if p.len() >= r.len() && p[..r.len()] == r[..] {
        Ok(path)
    } else {
        Err(format!("çalışma alanı dışı yol: {rel}"))
    }
}

/// Yakalanan süreç çıktısı.
struct Captured {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Python `repr(list)` biçiminde komut (zaman aşımı/CalledProcessError metni için).
fn cmd_repr(cmd: &Command) -> String {
    let mut parts = vec![py_repr(&cmd.get_program().to_string_lossy())];
    parts.extend(cmd.get_args().map(|a| py_repr(&a.to_string_lossy())));
    format!("[{}]", parts.join(", "))
}

/// `subprocess.run(..., capture_output=True, timeout=...)`: zaman aşımında süreç ağacı öldürülür.
fn run_capture(
    cmd: &mut Command,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Captured, String> {
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut tc = spawn_in_group(cmd).map_err(|e| e.to_string())?;
    let mut out_pipe = tc.child.stdout.take();
    let mut err_pipe = tc.child.stderr.take();
    let in_pipe = tc.child.stdin.take();
    let t_out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let t_err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    if let (Some(mut p), Some(data)) = (in_pipe, stdin) {
        std::thread::spawn(move || {
            let _ = p.write_all(&data);
        });
    }
    let code = tc.wait_timeout(timeout);
    let Some(code) = code else {
        tc.kill_tree();
        return Err(format!(
            "Command '{}' timed out after {} seconds",
            cmd_repr(cmd),
            timeout.as_secs()
        ));
    };
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();
    Ok(Captured {
        code,
        stdout,
        stderr,
    })
}

/// `subprocess.run(cmd, shell=True)` komutu.
fn shell_command(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        let mut c = Command::new(comspec);
        c.raw_arg(format!("/c \"{cmd}\""));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(cmd);
        c
    }
}

pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Git Bash tercih edilir: gizli/arka plan süreçlerinde PATH'teki `bash`
/// Windows'un WSL yönlendiricisi (System32\bash.exe) olabilir.
fn find_bash() -> Option<PathBuf> {
    if cfg!(windows) {
        for cand in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ] {
            if Path::new(cand).is_file() {
                return Some(PathBuf::from(cand));
            }
        }
        let found = which("bash")?;
        if found
            .display()
            .to_string()
            .to_lowercase()
            .replace('/', "\\")
            .contains("system32")
        {
            return None;
        }
        return Some(found);
    }
    which("bash")
}

fn op_clone_tree(step: &Step, root: &Path, dry: bool) -> Result<String, String> {
    let mut args = serde_json::Map::new();
    for k in ["src", "dst", "replacements"] {
        args.insert(k.into(), step.get(k).cloned().ok_or(format!("'{k}'"))?);
    }
    args.insert(
        "exclude".into(),
        step.get("exclude").cloned().unwrap_or(Value::Null),
    );
    args.insert("dry_run".into(), Value::Bool(dry));
    Ok(tool_clone_tree(
        &ToolContext::load(root),
        &Value::Object(args),
    ))
}

/// Python `re.sub` şablonunu fancy-regex şablonuna çevirir.
fn py_template(repl: &str) -> String {
    let mut out = String::new();
    let mut it = repl.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '$' => out.push_str("$$"),
            '\\' => match it.next() {
                None => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('f') => out.push('\x0c'),
                Some('v') => out.push('\x0b'),
                Some('a') => out.push('\x07'),
                Some('\\') => out.push('\\'),
                Some('g') if it.peek() == Some(&'<') => {
                    it.next();
                    let name: String = it.by_ref().take_while(|c| *c != '>').collect();
                    out.push_str(&format!("${{{name}}}"));
                }
                Some(d) if d.is_ascii_digit() => {
                    let mut num = d.to_string();
                    if let Some(d2) = it.peek().copied().filter(char::is_ascii_digit) {
                        it.next();
                        num.push(d2);
                    }
                    out.push_str(&format!("${{{num}}}"));
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            },
            _ => out.push(c),
        }
    }
    out
}

fn has_magic(seg: &str) -> bool {
    seg.contains(['*', '?', '['])
}

/// `sorted(root.glob(pattern))` (pathlib; Windows'ta büyük/küçük harf duyarsız).
fn glob_sorted(root: &Path, pattern: &str) -> Vec<PathBuf> {
    fn walk_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
        out.push(dir.to_path_buf());
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut subs: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        subs.sort();
        for s in subs {
            walk_dirs(&s, out);
        }
    }
    let mut cur = vec![root.to_path_buf()];
    for seg in pattern
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
    {
        let mut next = Vec::new();
        for p in &cur {
            if seg == "**" {
                walk_dirs(p, &mut next);
            } else if has_magic(seg) {
                if let Ok(rd) = std::fs::read_dir(p) {
                    for e in rd.flatten() {
                        if fnmatch(&e.file_name().to_string_lossy(), seg) {
                            next.push(e.path());
                        }
                    }
                }
            } else {
                let q = p.join(seg);
                if q.exists() {
                    next.push(q);
                }
            }
        }
        cur = next;
    }
    let mut keyed: Vec<(Vec<String>, PathBuf)> =
        cur.into_iter().map(|p| (norm_parts(&p), p)).collect();
    keyed.sort();
    keyed.dedup_by(|a, b| a.0 == b.0);
    keyed.into_iter().map(|(_, p)| p).collect()
}

fn op_replace(step: &Step, root: &Path, dry: bool) -> Result<String, String> {
    let find = req_str(step, "find")?;
    let repl = req_str(step, "with")?;
    let mut flags = String::new();
    for c in str_of(step, "flags").unwrap_or_default().chars() {
        match c {
            's' | 'i' | 'm' => flags.push(c),
            other => return Err(format!("'{other}'")),
        }
    }
    let is_regex = flag(step, "regex", false);
    let find = if is_regex {
        find.replace("\\n", "\\r?\\n") // CRLF dosyalarda satır sonu da eşleşir
    } else {
        find.split('\n')
            .map(fancy_regex::escape)
            .collect::<Vec<_>>()
            .join("\\r?\\n")
    };
    let full = if flags.is_empty() {
        find
    } else {
        format!("(?{flags}){find}")
    };
    let pat = fancy_regex::Regex::new(&full).map_err(|e| e.to_string())?;
    let template = py_template(&repl);
    let glob = req_str(step, "glob")?;
    let (mut files, mut changed) = (0, 0);
    for path in glob_sorted(root, &glob) {
        if !path.is_file()
            || path.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some(".git" | "bin" | "obj" | "node_modules")
                )
            })
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        files += 1;
        let new = if is_regex {
            pat.try_replacen(&text, 0, template.as_str())
        } else {
            pat.try_replacen(&text, 0, fancy_regex::NoExpand(&repl))
        }
        .map_err(|e| e.to_string())?;
        if new != text {
            changed += 1;
            if !dry {
                std::fs::write(&path, new.as_bytes()).map_err(|e| e.to_string())?;
            }
        }
    }
    if flag(step, "required", true) && !dry && changed == 0 {
        return Err(format!(
            "replace hicbir dosyada eslesmedi ({files} dosya tarandi); 'required': false ile atlanabilir"
        ));
    }
    Ok(format!("{files} dosya tarandı, {changed} değişti"))
}

fn op_write(step: &Step, root: &Path, dry: bool) -> Result<String, String> {
    let path = resolve_in_root(root, &req_str(step, "path")?)?;
    if !dry {
        let content = req_str(step, "content")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
    }
    Ok(format!("yazıldı: {}", path.display()))
}

fn op_run(step: &Step, root: &Path, dry: bool) -> Result<String, String> {
    let cmd = req_str(step, "cmd")?;
    if dry {
        return Ok(format!("çalıştırılacak: {cmd}"));
    }
    let bash = if str_of(step, "shell").unwrap_or_else(|| "bash".into()) == "bash" {
        find_bash()
    } else {
        None
    };
    let mut c = match &bash {
        Some(b) => {
            let mut c = Command::new(b);
            c.arg("-c").arg(&cmd);
            c
        }
        None => shell_command(&cmd),
    };
    c.current_dir(resolve_in_root(
        root,
        &str_of(step, "cwd").unwrap_or_else(|| ".".into()),
    )?);
    let secs = int_or(step, "timeout", 900).max(0) as u64;
    let r = run_capture(&mut c, None, Duration::from_secs(secs))?;
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );
    let tail = tail(&all, 12);
    if !ok_exit(step, r.code) {
        return Err(format!("çıkış kodu {}\n{tail}", r.code));
    }
    let stdout = String::from_utf8_lossy(&r.stdout);
    if let Some(rel) = str_of(step, "save").filter(|s| !s.is_empty()) {
        // Ölçüm özeti dosyaya: sonraki agent adımı `inputs` ile rakamları hazır alır.
        let path = resolve_in_root(root, &rel)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, stdout.as_bytes()).map_err(|e| e.to_string())?;
    }
    let warn = check_count(step, &stdout)?;
    Ok(match tail.lines().last() {
        Some(last) => format!("tamam: {last}{warn}"),
        None => format!("tamam{warn}"),
    })
}

/// Sayaç doğrulaması: ayrıştırma bozulup sessizce 0 dönen sayaçlar yakalanır.
/// `min_count`: çıktıdaki sayı (`count_regex`'in ilk grubu ya da son tam sayı) en az bu olmalı;
/// sayı yoksa ya da azsa HATA (`count_warn: true` ile uyarı). `min_count` yoksa ve çıktı
/// yalnız `0` ise uyarı eklenir.
fn check_count(step: &Step, stdout: &str) -> Result<String, String> {
    let Some(min) = step.get("min_count").and_then(Value::as_i64) else {
        return Ok(if stdout.trim() == "0" {
            " [UYARI: sayaç 0 döndü; ayrıştırma gerçekten bir şey buldu mu? `min_count` ile doğrula]"
                .into()
        } else {
            String::new()
        });
    };
    let problem = match count_in(step, stdout)? {
        None => "sayaç sayı üretmedi (çıktı ayrıştırılamadı)".to_owned(),
        Some(v) if v < min => {
            format!("sayaç {v} < beklenen en az {min} (ayrıştırma/desen hatası olabilir)")
        }
        Some(_) => return Ok(String::new()),
    };
    if flag(step, "count_warn", false) {
        Ok(format!(" [UYARI: {problem}]"))
    } else {
        Err(problem)
    }
}

fn count_in(step: &Step, stdout: &str) -> Result<Option<i64>, String> {
    static LAST_INT: OnceLock<Option<Regex>> = OnceLock::new();
    let found = match str_of(step, "count_regex").filter(|s| !s.is_empty()) {
        Some(p) => {
            let re = Regex::new(&p).map_err(|e| format!("count_regex: {e}"))?;
            re.captures_iter(stdout)
                .last()
                .and_then(|c| c.get(1).or_else(|| c.get(0)))
                .map(|m| m.as_str().trim().to_owned())
        }
        None => LAST_INT
            .get_or_init(|| Regex::new(r"-?\d+").ok())
            .as_ref()
            .and_then(|re| re.find_iter(stdout).last())
            .map(|m| m.as_str().to_owned()),
    };
    Ok(found.and_then(|s| s.parse().ok()))
}

fn ai_signature() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)co-authored-by|generated with|claude").ok())
        .as_ref()
}

fn git_cmd(repo: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(args);
    c
}

/// `subprocess.run(..., check=True)`: sıfır dışı çıkışta CalledProcessError metni.
fn git_check(repo: &Path, args: &[&str]) -> Result<Captured, String> {
    let mut c = git_cmd(repo, args);
    let r = run_capture(&mut c, None, Duration::from_secs(3600))?;
    if r.code != 0 {
        return Err(format!(
            "Command '{}' returned non-zero exit status {}.",
            cmd_repr(&c),
            r.code
        ));
    }
    Ok(r)
}

fn git_out(repo: &Path, args: &[&str]) -> String {
    let mut c = git_cmd(repo, args);
    run_capture(&mut c, None, Duration::from_secs(3600))
        .map(|r| String::from_utf8_lossy(&r.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn op_commit(step: &Step, root: &Path, dry: bool) -> Result<String, String> {
    let msg = req_str(step, "message")?;
    if ai_signature().is_some_and(|re| re.is_match(&msg)) {
        return Err("commit mesajında AI imzası/attribution yasak (proje kuralı)".into());
    }
    if dry {
        return Ok(format!("commit: {}", first_chars(&msg, 60)));
    }
    let repo = resolve_in_root(root, &req_str(step, "repo")?)?;
    git_check(&repo, &["add", "-A"])?;
    // Mesaj stdin'den utf-8 verilir: Windows'ta argv üzerinden geçen Türkçe karakterler çift kodlanıyordu.
    let mut c = git_cmd(&repo, &["commit", "-q", "-F", "-"]);
    let r = run_capture(&mut c, Some(msg.into_bytes()), Duration::from_secs(3600))?;
    let mut all = r.stdout;
    all.extend_from_slice(&r.stderr);
    Ok(if r.code == 0 {
        "commit atıldı".into()
    } else {
        format!(
            "commit yok: {}",
            first_chars(String::from_utf8_lossy(&all).trim(), 120)
        )
    })
}

fn op_push(step: &Step, root: &Path, dry: bool, allow_push: bool) -> Result<String, String> {
    if !allow_push {
        return Err("push adımı için --allow-push gerekir (kullanıcı onayı)".into());
    }
    if dry {
        return Ok("push".into());
    }
    let repo = resolve_in_root(root, &req_str(step, "repo")?)?;
    let mut branch = str_of(step, "branch")
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    if branch.is_empty() {
        branch = git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    }
    if branch.is_empty() {
        branch = "main".into();
    }
    let target = str_of(step, "target")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| branch.clone());
    let pending = push_guard(step, &repo, &branch, &target)?;
    let refspec = if target == branch {
        branch.clone()
    } else {
        format!("{branch}:{target}")
    };
    let mut c = git_cmd(&repo, &["push", "origin", &refspec]);
    let r = run_capture(&mut c, None, Duration::from_secs(3600))?;
    if r.code != 0 {
        return Err(last_chars(String::from_utf8_lossy(&r.stderr).trim(), 300).to_owned());
    }
    let mut msg = format!("push edildi ({pending} commit → {target})");
    if flag(step, "drone", true) {
        let wait = step
            .get("drone_wait")
            .and_then(Value::as_f64)
            .unwrap_or(90.0);
        let (text, last) = drone_trigger(&repo, &branch, wait);
        msg.push_str("; ");
        msg.push_str(&text);
        if flag(step, "drone_result", true) {
            if let Some((slug, number)) = last.filter(|(_, n)| py_truthy(n)) {
                let t = step
                    .get("drone_timeout")
                    .and_then(Value::as_f64)
                    .unwrap_or(1500.0);
                msg.push_str("; ");
                msg.push_str(&drone_await(&slug, &number, t)?);
            }
        }
    }
    Ok(msg)
}

/// Dal güvenliği: `origin/<target>..<branch>` (uzak dal yoksa `<base>..<branch>`) ile
/// gönderilecek commit'leri listeler. Yazarı yerel `user.email` olmayan commit varsa
/// (ör. `test` dalı `main`'in önündeyken başkasının işi prod'a taşınacaksa) durur ve
/// kendi commit'lerini cherry-pick etmeyi önerir; `allow_foreign: true` ile geçilir.
fn push_guard(step: &Step, repo: &Path, branch: &str, target: &str) -> Result<usize, String> {
    git_out(repo, &["fetch", "-q", "origin", target]);
    let remote = format!("origin/{target}");
    let range = if git_cmd(repo, &["rev-parse", "--verify", "-q", &remote])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {
        format!("{remote}..{branch}")
    } else {
        let base = str_of(step, "base")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "main".into());
        format!("{base}..{branch}")
    };
    let me = git_out(repo, &["config", "user.email"]).to_lowercase();
    let log = git_out(
        repo,
        &["log", "--no-merges", "--format=%h\t%ae\t%an\t%s", &range],
    );
    let commits: Vec<(&str, &str, &str, &str)> = log
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(4, '\t');
            Some((it.next()?, it.next()?, it.next()?, it.next().unwrap_or("")))
        })
        .collect();
    for (h, _, an, s) in &commits {
        say(&format!("  push {range}: {h} {an}: {}", first_chars(s, 70)));
    }
    let foreign: Vec<_> = commits
        .iter()
        .filter(|(_, ae, _, _)| !me.is_empty() && ae.to_lowercase() != me)
        .collect();
    if foreign.is_empty() || flag(step, "allow_foreign", false) {
        return Ok(commits.len());
    }
    let own: Vec<&str> = commits
        .iter()
        .filter(|(_, ae, _, _)| ae.to_lowercase() == me)
        .map(|(h, ..)| *h)
        .rev()
        .collect();
    let list: Vec<String> = foreign
        .iter()
        .map(|(h, _, an, s)| format!("{h} {an}: {}", first_chars(s, 60)))
        .collect();
    Err(format!(
        "{range} içinde başkasına ait {} commit var, push durduruldu:\n  {}\n\
         Yalnız kendi işini göndermek için: git switch -c evren/temiz {remote} && git cherry-pick {} \
         ve push'u o dalla yap; bilerek gönderiyorsan adıma \"allow_foreign\": true ekle.",
        foreign.len(),
        list.join("\n  "),
        if own.is_empty() { "<commit>".to_owned() } else { own.join(" ") }
    ))
}

fn drone_server() -> String {
    std::env::var("DRONE_SERVER")
        .unwrap_or_else(|_| "https://drone.sentayazilimtest.space".into())
        .trim_end_matches('/')
        .to_owned()
}

/// urllib `HTTPError` metni.
fn http_error(status: reqwest::StatusCode) -> String {
    format!(
        "HTTP Error {}: {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
}

struct Drone {
    client: reqwest::blocking::Client,
    server: String,
    token: String,
}

impl Drone {
    fn new(token: String) -> Result<Self, String> {
        // Drone önündeki katman varsayılan UA'ları 403'ler.
        let client = reqwest::blocking::Client::builder()
            .user_agent("curl/8.0")
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Drone {
            client,
            server: drone_server(),
            token,
        })
    }

    fn get(&self, path: &str, secs: u64) -> Result<Value, (Option<u16>, String)> {
        let resp = self
            .client
            .get(format!("{}{path}", self.server))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(secs))
            .send()
            .map_err(|e| (None, e.to_string()))?;
        if resp.status().as_u16() >= 400 {
            return Err((Some(resp.status().as_u16()), http_error(resp.status())));
        }
        resp.json::<Value>().map_err(|e| (None, e.to_string()))
    }

    fn post(&self, path: &str) -> Result<Value, (Option<u16>, String)> {
        let resp = self
            .client
            .post(format!("{}{path}", self.server))
            .bearer_auth(&self.token)
            .body(Vec::new())
            .timeout(Duration::from_secs(60))
            .send()
            .map_err(|e| (None, e.to_string()))?;
        if resp.status().as_u16() >= 400 {
            return Err((Some(resp.status().as_u16()), http_error(resp.status())));
        }
        let body = resp.text().map_err(|e| (None, e.to_string()))?;
        Ok(if body.starts_with('{') {
            serde_json::from_str(&body).map_err(|e| (None, e.to_string()))?
        } else {
            Value::Object(serde_json::Map::new())
        })
    }
}

/// Push sonrası Drone build'ini API'den tetikler (GitHub webhook gecikmesine
/// bağımlılık kalmaz). DRONE_TOKEN yoksa atlanır; hata push'u düşürmez.
fn drone_trigger(repo: &Path, branch: &str, wait_s: f64) -> (String, Option<(String, Value)>) {
    let Some(tok) = std::env::var("DRONE_TOKEN").ok().filter(|t| !t.is_empty()) else {
        return ("Drone tetiklenmedi (DRONE_TOKEN yok)".into(), None);
    };
    let url = git_out(repo, &["remote", "get-url", "origin"]);
    let slug = Regex::new(r"github\.com[:/]([^/]+)/([^/\s]+?)(?:\.git)?$")
        .ok()
        .and_then(|re| re.captures(&url).map(|m| format!("{}/{}", &m[1], &m[2])));
    let Some(slug) = slug else {
        return ("Drone tetiklenmedi (origin GitHub değil)".into(), None);
    };
    let sha = git_out(repo, &["rev-parse", "HEAD"]);
    let run = || -> Result<(String, Option<(String, Value)>), String> {
        let d = Drone::new(tok.clone())?;
        // Önce webhook'un getirdiği build'i bekle (çift build olmasın); süre dolunca API'den tetikle.
        let deadline = Instant::now() + Duration::from_secs_f64(wait_s.max(0.0));
        while Instant::now() < deadline {
            let builds = d
                .get(&format!("/api/repos/{slug}/builds?per_page=5"), 30)
                .map_err(|e| e.1)?;
            for b in builds.as_array().into_iter().flatten() {
                if b.get("after").and_then(Value::as_str) == Some(sha.as_str()) {
                    let n = b.get("number").cloned().unwrap_or(Value::Null);
                    return Ok((
                        format!("Drone build #{} webhook ile geldi ({slug})", py_str(&n)),
                        Some((slug.clone(), n)),
                    ));
                }
            }
            std::thread::sleep(Duration::from_secs(10));
        }
        let trig = format!("/api/repos/{slug}/builds?branch={branch}&commit={sha}");
        let (n, how) = match d.post(&trig) {
            Ok(v) => (v.get("number").cloned(), "API ile tetiklendi"),
            // 404 "Bad credentials"/401/500: repo sahibinin GitHub token'ı geçersiz; chown ile onarılıp yeniden denenir.
            Err((Some(401 | 404 | 500), _)) => {
                d.post(&format!("/api/repos/{slug}/chown"))
                    .map_err(|e| e.1)?;
                (
                    d.post(&trig).map_err(|e| e.1)?.get("number").cloned(),
                    "repo sahipliği (chown) onarılıp API ile tetiklendi",
                )
            }
            Err((_, e)) => return Err(e),
        };
        let n = n.unwrap_or(Value::Null);
        Ok((
            format!(
                "webhook {} sn içinde gelmedi, Drone build #{} {how} ({slug})",
                wait_s as i64,
                py_str(&n)
            ),
            Some((slug.clone(), n)),
        ))
    };
    match run() {
        Ok(r) => r,
        Err(e) => (
            format!("Drone tetiklenemedi: {}", first_chars(&e, 120)),
            None,
        ),
    }
}

/// Build bitene kadar bekler; failure/error/killed ise başarısız adımın log kuyruğuyla hata.
fn drone_await(slug: &str, number: &Value, timeout_s: f64) -> Result<String, String> {
    let d = Drone::new(std::env::var("DRONE_TOKEN").unwrap_or_default())?;
    let num = py_str(number);
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s.max(0.0));
    let mut build = Value::Object(serde_json::Map::new());
    while Instant::now() < deadline {
        match d.get(&format!("/api/repos/{slug}/builds/{num}"), 60) {
            Ok(b) => build = b,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(15));
                continue;
            }
        }
        let st = build.get("status").and_then(Value::as_str).unwrap_or("");
        if !["pending", "running", "blocked", "waiting_on_dependencies"].contains(&st) {
            break;
        }
        std::thread::sleep(Duration::from_secs(15));
    }
    let status = build
        .get("status")
        .map(py_str)
        .unwrap_or_else(|| "bilinmiyor".into());
    if status == "success" {
        return Ok(format!("Drone build #{num} BAŞARILI ({slug})"));
    }
    if ["pending", "running", "bilinmiyor"].contains(&status.as_str()) {
        return Ok(format!(
            "Drone build #{num} {} sn içinde bitmedi (durum: {status}); elle izle ({slug})",
            timeout_s as i64
        ));
    }
    let mut detail = String::new();
    'stages: for st in build
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for sp in st
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let sst = sp.get("status").and_then(Value::as_str).unwrap_or("");
            if ["failure", "error", "killed"].contains(&sst) {
                let name = sp.get("name").map(py_str).unwrap_or_else(|| "None".into());
                let path = format!(
                    "/api/repos/{slug}/builds/{num}/logs/{}/{}",
                    st.get("number")
                        .map(py_str)
                        .unwrap_or_else(|| "None".into()),
                    sp.get("number")
                        .map(py_str)
                        .unwrap_or_else(|| "None".into())
                );
                detail = match d.get(&path, 60) {
                    Ok(lines) => {
                        let joined: String = lines
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|x| x.get("out").and_then(Value::as_str))
                            .collect();
                        let ls: Vec<&str> = joined.lines().collect();
                        format!(
                            "\nadım '{name}':\n{}",
                            ls[ls.len().saturating_sub(25)..].join("\n")
                        )
                    }
                    Err(_) => format!("\nadım '{name}' (log alınamadı)"),
                };
                break 'stages;
            }
        }
    }
    Err(format!(
        "Drone build #{num} {} ({slug}){detail}",
        status.to_uppercase()
    ))
}

/// agent `push: true`: eşzamanlı ajanların pushları sıralanır.
fn locked_push(step: &Step, root: &Path, dry: bool, allow_push: bool) -> Result<String, String> {
    let mut s = Step::new();
    s.insert("repo".into(), step.get("repo").cloned().ok_or("'repo'")?);
    for k in [
        "branch",
        "drone",
        "drone_wait",
        "drone_result",
        "drone_timeout",
    ] {
        if let Some(v) = step.get(k) {
            s.insert(k.into(), v.clone());
        }
    }
    let _g = lock(&PUSH_LOCK);
    op_push(&s, root, dry, allow_push)
}

/// `~/.evren/secrets.env` (KEY=VALUE satırları) ortamı doldurur; var olanlar ezilmez.
fn load_secrets() -> usize {
    let Some(path) = dirs::home_dir().map(|h| h.join(".evren").join("secrets.env")) else {
        return 0;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let mut n = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if !k.is_empty() && std::env::var_os(k).is_none() {
            std::env::set_var(k, v.trim().trim_matches(['\'', '"']));
            n += 1;
        }
    }
    n
}

/// Plan çalıştırıcısının ajan ve push ortamı.
fn runtime(root: &Path, dry: bool, allow_push: bool, runner: Box<dyn AgentRunner>) -> Runtime {
    let root = root.to_path_buf();
    Runtime {
        exe: vec![std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "evren".into())],
        runner,
        push_hook: Some(Box::new(move |s: &Step| {
            locked_push(s, &root, dry, allow_push)
        })),
        slots_dir: dirs::home_dir()
            .unwrap_or_default()
            .join(".evren")
            .join("slots"),
        skills_dir: evren_core::catalog::user_skills_dir(&evren_core::project_docs::cli_docs_dir(
            &evren_core::config::Config::load(),
        )),
    }
}

/// `evren plan`: planı çalıştırır; çıkış kodu (0 tamam, 1 hata).
pub fn run_plan(
    plan_path: &str,
    workspace: &Path,
    dry: bool,
    allow_push: bool,
    start: usize,
) -> Result<i32, String> {
    let root = resolve_path(workspace);
    let rt = runtime(&root, dry, allow_push, Box::new(ProcessRunner));
    run_plan_with(plan_path, &root, dry, allow_push, start, &rt)
}

fn run_plan_with(
    plan_path: &str,
    root: &Path,
    dry: bool,
    allow_push: bool,
    start: usize,
    rt: &Runtime,
) -> Result<i32, String> {
    load_secrets();
    let raw = std::fs::read(plan_path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let plan: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let steps = plan
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .ok_or("'steps'")?;
    agent::validate_skills(
        &steps,
        &evren_core::catalog::SkillCatalog::load(&rt.skills_dir),
    )?;
    let stem = Path::new(plan_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = steps.len();
    let mut ctx = Ctx::new();
    let ops = |op: &str, s: &Step| -> Option<Result<String, String>> {
        Some(match op {
            "clone_tree" => op_clone_tree(s, root, dry),
            "replace" => op_replace(s, root, dry),
            "write" => op_write(s, root, dry),
            "run" => op_run(s, root, dry),
            "commit" => op_commit(s, root, dry),
            "agent" => agent::op_agent(s, root, dry, resolve_in_root, rt),
            "live_diff" => live_diff::op_live_diff(s, root, dry),
            _ => return None,
        })
    };
    for (idx, raw_step) in steps.iter().enumerate() {
        let i = idx + 1;
        if i < start {
            continue;
        }
        let op = raw_step
            .get("op")
            .map(py_str)
            .unwrap_or_else(|| "None".into());
        let mut step = expand(raw_step, &ctx)
            .as_object()
            .cloned()
            .unwrap_or_default();
        step.insert("_label".into(), Value::String(format!("{stem}-{i}")));
        let res = if op == "parallel" {
            agent::op_parallel(&step, dry, &ops, &ctx, &rt.slots_dir)
        } else if op == "push" {
            op_push(&step, root, dry, allow_push)
        } else if REMOTE_OPS.contains(&op.as_str()) {
            run_remote(&op, &step, root, dry, &mut ctx)
                .unwrap_or_else(|| Err(format!("bilinmeyen adım: {op}")))
        } else {
            ops(&op, &step).unwrap_or_else(|| Err(format!("bilinmeyen adım: {op}")))
        };
        match res {
            Ok(out) => say(&format!("[{i}/{n}] {op}: {out}")),
            Err(e) => {
                say(&format!(
                    "[{i}/{n}] {op} HATA: {e}\nDevam için: evren plan {plan_path} --from {i}"
                ));
                return Ok(1);
            }
        }
    }
    say(&format!(
        "PLAN TAMAMLANDI{}",
        if dry { " (kuru çalıştırma)" } else { "" }
    ));
    Ok(0)
}

#[cfg(test)]
mod tests;
