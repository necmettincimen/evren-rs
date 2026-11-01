//! Güvenli dosya işlemleri (Python `file_ops.py`): çalışma dizini sınırı,
//! yasak klasörler, korunan dosyalar, yedek/geri alma, BOM/CRLF korunarak yazma.

use std::collections::HashSet;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use regex::Regex;

/// Erişimi engellenen sistem/bağımlılık klasörleri.
pub const FORBIDDEN_DIRS: &[&str] = &[".git", ".venv", "venv", "node_modules", "__pycache__"];
/// Doğrudan üzerine yazılamayan kritik ortam dosyaları.
pub const PROTECTED_FILES: &[&str] = &[".env", ".env.local"];

/// Yedek klasörü (çalışma dizinine göre).
pub const BACKUP_DIR: &str = ".evren/backups";

static BACKUP_ENABLED: OnceLock<AtomicBool> = OnceLock::new();

fn backup_flag() -> &'static AtomicBool {
    BACKUP_ENABLED.get_or_init(|| {
        let raw = std::env::var("EVREN_BACKUP").unwrap_or_else(|_| "false".to_string());
        let on = matches!(
            raw.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
        AtomicBool::new(on)
    })
}

/// Otomatik yedeklemeyi süreç genelinde açar/kapatır (`--backup`).
pub fn set_backup_enabled(enabled: bool) {
    backup_flag().store(enabled, Ordering::SeqCst);
}

/// Otomatik yedekleme açık mı?
pub fn backup_enabled() -> bool {
    backup_flag().load(Ordering::SeqCst)
}

/// Ek salt-okunur kökler (`EVREN_READ_ROOTS`, `;`/`:` ile ayrılmış).
pub fn read_roots() -> Vec<PathBuf> {
    let raw = std::env::var("EVREN_READ_ROOTS").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    raw.split(sep)
        .filter(|p| !p.trim().is_empty())
        .map(|p| resolve_path(Path::new(p)))
        .collect()
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    p
}

fn normalize_lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir) | Some(Component::Prefix(_))
                ) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `Path.resolve()` eşdeğeri: var olan en uzun ata canonicalize edilir,
/// kalan parçalar sözcüksel olarak eklenir; `\\?\` öneki kaldırılır.
pub fn resolve_path(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    let abs = normalize_lexical(&abs);
    if let Ok(c) = fs::canonicalize(&abs) {
        return strip_verbatim(c);
    }
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = abs.clone();
    while let Some(parent) = cur.parent().map(Path::to_path_buf) {
        if let Some(name) = cur.file_name() {
            rest.push(name.to_os_string());
        }
        if let Ok(c) = fs::canonicalize(&parent) {
            let mut out = strip_verbatim(c);
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        cur = parent;
    }
    abs
}

/// Git Bash biçimi `/d/Github/x` -> `D:/Github/x` (yalnız Windows'ta).
pub fn msys_to_windows(path_str: &str) -> String {
    if cfg!(windows) {
        static RE: OnceLock<Option<Regex>> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"^/([a-zA-Z])(/.*)?$").ok());
        if let Some(m) = re.as_ref().and_then(|r| r.captures(path_str)) {
            let drive = m
                .get(1)
                .map(|d| d.as_str().to_uppercase())
                .unwrap_or_default();
            let rest = m.get(2).map(|r| r.as_str()).unwrap_or("/");
            return format!("{drive}:{rest}");
        }
    }
    path_str.to_string()
}

/// Metin yolu çalışma dizinine göre çözümler.
pub fn resolve_workspace_path(path_str: &str, root: &Path) -> PathBuf {
    let root = resolve_path(root);
    let p = PathBuf::from(msys_to_windows(path_str));
    if p.is_absolute() {
        resolve_path(&p)
    } else {
        resolve_path(&root.join(p))
    }
}

/// Hedef yol çalışma dizini içinde ve yasak klasör dışında mı? `(güvenli, neden)`.
pub fn is_safe_path(target: &Path, root: &Path, read_only: bool) -> (bool, String) {
    let root = resolve_path(root);
    let resolved = if target.is_absolute() {
        resolve_path(target)
    } else {
        resolve_path(&root.join(target))
    };
    let mut allowed = vec![root.clone()];
    if read_only {
        allowed.extend(read_roots());
    }
    if !allowed
        .iter()
        .any(|base| resolved == *base || resolved.starts_with(base))
    {
        return (
            false,
            format!(
                "Güvenlik İhlali: Hedef yol ({}) çalışma dizini ({}) dışında.",
                resolved.display(),
                root.display()
            ),
        );
    }
    for comp in resolved.components() {
        let part = comp.as_os_str().to_string_lossy();
        if FORBIDDEN_DIRS.contains(&part.as_ref()) {
            return (
                false,
                format!(
                    "Güvenlik Uyarısı: '{part}' sistem/bağımlılık klasörüne erişim engellendi."
                ),
            );
        }
    }
    (true, String::new())
}

fn backup_rel_name(target: &Path, root: &Path) -> String {
    let rel = target.strip_prefix(root).unwrap_or(target);
    rel.to_string_lossy().replace(['/', '\\'], "_")
}

fn timestamp_now() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

/// `.evren/backups/<rel>.<zaman>.bak` yedeği oluşturur.
pub fn backup_file(target: &Path, root: &Path) -> std::io::Result<PathBuf> {
    let root = resolve_path(root);
    let backup_root = root.join(BACKUP_DIR);
    fs::create_dir_all(&backup_root)?;
    let name = format!("{}.{}.bak", backup_rel_name(target, &root), timestamp_now());
    let dest = backup_root.join(name);
    fs::copy(target, &dest)?;
    Ok(dest)
}

/// En son yedeği geri yükler; `(başarı, mesaj)`.
pub fn rollback_last_backup(target: &Path, root: &Path) -> (bool, String) {
    let root = resolve_path(root);
    let backup_root = root.join(BACKUP_DIR);
    if !backup_root.exists() {
        return (
            false,
            "Yedek klasörü (.evren/backups) bulunamadı.".to_string(),
        );
    }
    let prefix = format!("{}.", backup_rel_name(target, &root));
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut backups: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&backup_root)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with(&prefix) && n.ends_with(".bak")
                })
                .filter_map(|e| {
                    let mtime = e.metadata().ok()?.modified().ok()?;
                    Some((mtime, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    backups.sort_by_key(|a| std::cmp::Reverse(a.0));
    let Some((_, latest)) = backups.into_iter().next() else {
        return (
            false,
            format!("{file_name} için geri yüklenecek yedek bulunamadı."),
        );
    };
    if let Err(e) = fs::copy(&latest, target) {
        return (false, format!("Dosya yazma hatası: {e}"));
    }
    let backup_name = latest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (
        true,
        format!("{file_name} dosyası {backup_name} yedeğinden geri yüklendi."),
    )
}

/// Dosyanın yazım biçimi: BOM var mı, baskın satır sonu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileMeta {
    pub has_bom: bool,
    pub crlf: bool,
}

impl FileMeta {
    /// Python `meta["newline"]` eşdeğeri.
    pub fn newline(&self) -> &'static str {
        if self.crlf {
            "\r\n"
        } else {
            "\n"
        }
    }
}

/// Varsayılan okuma sınırı (karakter).
pub const READ_MAX_CHARS: usize = 150_000;

/// Metin dosyasını okur; `max_chars=0` sınırsızdır.
pub fn read_file_with_meta(
    target: &str,
    root: &Path,
    max_chars: usize,
) -> Result<(String, FileMeta), String> {
    let root = resolve_path(root);
    let resolved = resolve_workspace_path(target, &root);
    let (safe, reason) = is_safe_path(&resolved, &root, true);
    if !safe {
        return Err(reason);
    }
    if !resolved.exists() {
        return Err(format!("Dosya bulunamadı: {}", resolved.display()));
    }
    if !resolved.is_file() {
        return Err(format!(
            "Belirtilen yol bir dosya değil: {}",
            resolved.display()
        ));
    }
    let mut raw = fs::read(&resolved).map_err(|e| format!("Dosya okuma hatası: {e}"))?;
    let has_bom = raw.starts_with(&[0xEF, 0xBB, 0xBF]);
    if has_bom {
        raw.drain(..3);
    }
    let mut text = String::from_utf8_lossy(&raw).into_owned();
    let meta = FileMeta {
        has_bom,
        crlf: text.contains("\r\n"),
    };
    if max_chars > 0 && text.chars().count() > max_chars {
        let cut: String = text.chars().take(max_chars).collect();
        text = format!("{cut}\n\n... [Dosya çok büyük olduğu için {max_chars} karakterde kesildi]");
    }
    Ok((text, meta))
}

/// Yalnız içerik (hata metniyle).
pub fn safe_read_file(target: &str, root: &Path, max_chars: usize) -> Result<String, String> {
    read_file_with_meta(target, root, max_chars).map(|(t, _)| t)
}

/// Geçici dosya + yeniden adlandırma ile yazar (yarım dosya kalmaz).
pub fn atomic_write_text(path: &Path, data: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::Builder::new()
        .prefix(".evren-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    let (mut file, tmp_path) = tmp.keep().map_err(|e| e.error)?;
    let result = file.write_all(data.as_bytes()).and_then(|_| file.flush());
    drop(file);
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp_path, path) {
        // Windows'ta hedef açıkken rename başarısız olabilir: kopyala.
        if fs::copy(&tmp_path, path).is_err() {
            let _ = fs::remove_file(&tmp_path);
            return Err(e);
        }
        let _ = fs::remove_file(&tmp_path);
    }
    Ok(())
}

/// Python `str.splitlines(keepends=True)` (yalnız `\n`, `\r\n`, `\r`).
pub fn split_lines_keepends(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&text[start..=i]);
                start = i + 1;
            }
            b'\r' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 1;
                }
                out.push(&text[start..=i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < bytes.len() {
        out.push(&text[start..]);
    }
    out
}

/// Python `str.splitlines()` (satır sonları atılır).
pub fn split_lines(text: &str) -> Vec<&str> {
    split_lines_keepends(text)
        .into_iter()
        .map(|l| l.trim_end_matches('\n').trim_end_matches('\r'))
        .collect()
}

/// Değişen satır aralığı (0 tabanlı, bitiş hariç): `(eski_bas, eski_son, yeni_bas, yeni_son)`.
pub fn changed_line_span(old: &str, new: &str) -> Option<(usize, usize, usize, usize)> {
    if old == new {
        return None;
    }
    let old_lines = split_lines_keepends(old);
    let new_lines = split_lines_keepends(new);
    let limit = old_lines.len().min(new_lines.len());
    let mut start = 0;
    while start < limit && old_lines[start] == new_lines[start] {
        start += 1;
    }
    let mut tail = 0;
    while tail < limit - start
        && old_lines[old_lines.len() - 1 - tail] == new_lines[new_lines.len() - 1 - tail]
    {
        tail += 1;
    }
    Some((start, old_lines.len() - tail, start, new_lines.len() - tail))
}

/// Yazma sonucu: yedek yolu (varsa).
pub type WriteResult = Result<Option<PathBuf>, (Option<PathBuf>, String)>;

fn protected_error(resolved: &Path) -> Option<String> {
    let name = resolved.file_name()?.to_string_lossy();
    PROTECTED_FILES
        .contains(&name.as_ref())
        .then(|| format!("'{name}' kritik ortam dosyası doğrudan üzerine yazılamaz."))
}

/// Yalnız değişen satır aralığını diske yazar; dosya bu arada değiştiyse tam yazar.
pub fn write_changed_lines(
    target: &str,
    old_content: &str,
    new_content: &str,
    root: &Path,
    preserve_meta: Option<&FileMeta>,
) -> WriteResult {
    let root = resolve_path(root);
    let resolved = resolve_workspace_path(target, &root);
    let Some((old_start, old_end, new_start, new_end)) =
        changed_line_span(old_content, new_content)
    else {
        return Ok(None);
    };
    let (safe, reason) = is_safe_path(&resolved, &root, false);
    if !safe {
        return Err((None, reason));
    }
    if let Some(msg) = protected_error(&resolved) {
        return Err((None, msg));
    }
    let old_lines = split_lines_keepends(old_content);
    let new_lines = split_lines_keepends(new_content);
    let mut head: String = old_lines[..old_start].concat();
    let tail: String = old_lines[old_end..].concat();
    let mut middle: String = new_lines[new_start..new_end].concat();
    if let Some(meta) = preserve_meta {
        if meta.crlf {
            middle = middle.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        if meta.has_bom {
            head.insert(0, '\u{feff}');
        }
    }
    let mut backup_path = None;
    if backup_enabled() {
        match backup_file(&resolved, &root) {
            Ok(p) => backup_path = Some(p),
            Err(e) => return Err((None, format!("Dosya yazma hatası: {e}"))),
        }
    }
    let raw = match fs::read(&resolved) {
        Ok(r) => r,
        Err(e) => return Err((backup_path, format!("Dosya yazma hatası: {e}"))),
    };
    let head_bytes = head.as_bytes();
    let tail_bytes = tail.as_bytes();
    if !raw.starts_with(head_bytes)
        || !raw.ends_with(tail_bytes)
        || raw.len() < head_bytes.len() + tail_bytes.len()
    {
        return safe_write_file(target, new_content, &root, Some(false), preserve_meta);
    }
    let write = || -> std::io::Result<()> {
        let mut fh = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&resolved)?;
        let offset = head_bytes.len() as u64;
        fh.seek(SeekFrom::Start(offset))?;
        fh.write_all(middle.as_bytes())?;
        fh.write_all(tail_bytes)?;
        fh.set_len(offset + middle.len() as u64 + tail_bytes.len() as u64)?;
        fh.flush()?;
        fh.sync_all()
    };
    match write() {
        Ok(()) => Ok(backup_path),
        Err(e) => Err((backup_path, format!("Dosya yazma hatası: {e}"))),
    }
}

/// Metni güvenli biçimde yazar; `create_backup=None` süreç bayrağını kullanır.
pub fn safe_write_file(
    target: &str,
    content: &str,
    root: &Path,
    create_backup: Option<bool>,
    preserve_meta: Option<&FileMeta>,
) -> WriteResult {
    let root = resolve_path(root);
    let resolved = resolve_workspace_path(target, &root);
    let (safe, reason) = is_safe_path(&resolved, &root, false);
    if !safe {
        return Err((None, reason));
    }
    if let Some(msg) = protected_error(&resolved) {
        return Err((None, msg));
    }
    let create_backup = create_backup.unwrap_or_else(backup_enabled);
    let mut backup_path = None;
    if resolved.exists() && create_backup {
        match backup_file(&resolved, &root) {
            Ok(p) => backup_path = Some(p),
            Err(e) => return Err((None, format!("Dosya yazma hatası: {e}"))),
        }
    }
    if let Some(parent) = resolved.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return Err((backup_path, format!("Dosya yazma hatası: {e}")));
        }
    }
    let mut data = content.to_string();
    if let Some(meta) = preserve_meta {
        if meta.crlf {
            data = data.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        if meta.has_bom {
            data.insert(0, '\u{feff}');
        }
    }
    match fs::write(&resolved, data.as_bytes()) {
        Ok(()) => Ok(backup_path),
        Err(e) => Err((backup_path, format!("Dosya yazma hatası: {e}"))),
    }
}

/// Yasak klasör adı mı? (dizin gezintisi için yardımcı)
pub fn is_forbidden_dir(name: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| FORBIDDEN_DIRS.iter().copied().collect())
        .contains(name)
}
