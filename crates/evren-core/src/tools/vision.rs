//! Görsel betimleme (7.7) ve anlamsal kod arama (7.8): Python `vision_search.py`.
//!
//! Her iki araç da EVREN API'sine (`/chat/completions`, `/embeddings`,
//! `/rerank`) bloklayan HTTP ile gider. HTTP katmanı [`Poster`] ile soyutlanır;
//! testler sahte bir poster verir.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::UNIX_EPOCH;

use serde_json::{json, Map, Value};

use super::file_ops::split_lines;
use super::ToolContext;
use crate::config::{Config, SslVerify};

pub const VISION_MODEL: &str = "qwen3-vl-30b";
pub const EMBED_MODEL: &str = "qwen3-embedding-8b";
pub const RERANK_MODEL: &str = "qwen3-reranker-8b";
pub const CHUNK_LINES: usize = 60;
pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_INDEX_FILE_BYTES: u64 = 300_000;
const EMBED_BATCH: usize = 32;
const EMBED_INPUT_CHARS: usize = 6000;
const CHUNK_STORE_CHARS: usize = 1500;
const TOP_K: usize = 20;
const RESULT_N: usize = 8;

/// Eşzamanlı gömme bu kadar parçanın üstünde adımı dakikalarca bloklar;
/// daha büyük işler arka plan iş parçacığına gider (`_SYNC_CHUNK_LIMIT`).
pub const SYNC_CHUNK_LIMIT: usize = 40;

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".evren",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".idea",
];

const TEXT_EXTS: &[&str] = &[
    "py", "js", "ts", "tsx", "jsx", "go", "rs", "java", "c", "h", "cpp", "cs", "rb", "php", "sh",
    "ps1", "md", "txt", "json", "yaml", "yml", "toml", "ini", "cfg", "html", "css", "sql",
];

// ---------------------------------------------------------------- ayarlar / HTTP

/// API hatası: Python'daki `type(e).__name__` + mesaj karşılığı.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub kind: String,
    pub message: String,
}

impl ApiError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        ApiError {
            kind: kind.to_string(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// `POST {base_url}{path}` JSON gövdesiyle; yanıt JSON'u döner. Argümanlar:
/// yol (`/embeddings`), gövde, zaman aşımı (sn).
pub type Poster = Arc<dyn Fn(&str, &Value, u64) -> Result<Value, ApiError> + Send + Sync>;

/// Görsel/anlamsal arama için API ayarları (`ToolContext::new` doldurur).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisionSettings {
    pub base_url: String,
    pub api_keys: Vec<String>,
    /// `EVREN_SEMANTIC_SEARCH` (varsayılan açık).
    pub semantic_search: bool,
}

impl VisionSettings {
    pub fn from_config(config: &Config) -> Self {
        let raw = config
            .get("EVREN_SEMANTIC_SEARCH")
            .unwrap_or("true")
            .trim()
            .to_lowercase();
        VisionSettings {
            base_url: config.base_url(),
            api_keys: config.api_keys(),
            semantic_search: matches!(raw.as_str(), "1" | "true" | "yes" | "on"),
        }
    }
}

/// API anahtar kaynağı. Ajan tarafında evren-api `ApiKeyPool` bu arabirimle
/// bağlanır (evren-core evren-api'ye bağımlı değildir); yoksa [`ListKeys`].
pub trait KeySource: Send + Sync {
    /// Kullanılacak anahtar; hazır anahtar yoksa `None`.
    fn acquire(&self) -> Option<String>;
    fn report_success(&self, key: &str);
    /// 429: anahtar soğumaya alınır, sıradaki denenir.
    fn report_rate_limited(&self, key: &str, retry_after: Option<f64>);
    /// 401/402/403: anahtar bu süreçte bir daha kullanılmaz.
    fn report_disabled(&self, key: &str, reason: &str);
}

/// Havuz bağlanmamışsa yapılandırmadaki anahtarlar sırayla denenir.
#[derive(Debug, Default)]
pub struct ListKeys {
    keys: Vec<String>,
    skipped: Mutex<Vec<String>>,
}

impl ListKeys {
    pub fn new(keys: Vec<String>) -> Self {
        ListKeys {
            keys,
            skipped: Mutex::new(Vec::new()),
        }
    }

    fn skip(&self, key: &str) {
        let mut skipped = self
            .skipped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !skipped.iter().any(|k| k == key) {
            skipped.push(key.to_string());
        }
    }
}

impl KeySource for ListKeys {
    fn acquire(&self) -> Option<String> {
        let skipped = self
            .skipped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.keys.iter().find(|k| !skipped.contains(k)).cloned()
    }

    fn report_success(&self, _key: &str) {}

    fn report_rate_limited(&self, key: &str, _retry_after: Option<f64>) {
        self.skip(key);
    }

    fn report_disabled(&self, key: &str, _reason: &str) {
        self.skip(key);
    }
}

/// Tek anahtarla yapılan isteğin sonucu.
#[derive(Debug)]
pub enum KeyAttempt {
    Done(Value),
    /// 429 (isteğe bağlı `Retry-After` saniyesi).
    RateLimited(Option<f64>, ApiError),
    /// 401/402/403: anahtar geçersiz ya da kotası bitmiş.
    Rejected(ApiError),
    /// Anahtardan bağımsız hata: başka anahtar denenmez.
    Failed(ApiError),
}

/// İsteği havuzdaki anahtarlarla sırayla dener (en çok `attempts` kez).
pub fn with_key_rotation(
    source: &dyn KeySource,
    attempts: usize,
    mut send: impl FnMut(&str) -> KeyAttempt,
) -> Result<Value, ApiError> {
    let mut last: Option<ApiError> = None;
    for _ in 0..attempts.max(1) {
        let Some(key) = source.acquire() else {
            break;
        };
        match send(&key) {
            KeyAttempt::Done(v) => {
                source.report_success(&key);
                return Ok(v);
            }
            KeyAttempt::RateLimited(retry_after, e) => {
                source.report_rate_limited(&key, retry_after);
                last = Some(e);
            }
            KeyAttempt::Rejected(e) => {
                source.report_disabled(&key, &e.message);
                last = Some(e);
            }
            KeyAttempt::Failed(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| ApiError::new("RuntimeError", "Kullanılabilir API anahtarı yok.")))
}

fn post_once(client: &reqwest::blocking::Client, url: &str, body: &[u8], key: &str) -> KeyAttempt {
    let resp = match client
        .post(url)
        .header("Authorization", format!("Bearer {key}"))
        .header("X-API-Key", key)
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            let kind = if e.is_timeout() {
                "ReadTimeout"
            } else {
                "ConnectError"
            };
            return KeyAttempt::Failed(ApiError::new(kind, e.to_string()));
        }
    };
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let err = ApiError::new("HTTPStatusError", format!("HTTP {status} — {url}"));
        return match status {
            429 => {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<f64>().ok());
                KeyAttempt::RateLimited(retry_after, err)
            }
            401..=403 => KeyAttempt::Rejected(err),
            _ => KeyAttempt::Failed(err),
        };
    }
    match resp.text() {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(v) => KeyAttempt::Done(v),
            Err(e) => KeyAttempt::Failed(ApiError::new("JSONDecodeError", e.to_string())),
        },
        Err(e) => KeyAttempt::Failed(ApiError::new("ReadError", e.to_string())),
    }
}

/// Gerçek HTTP poster: `Authorization: Bearer` + `X-API-Key`; anahtar `keys`
/// kaynağından alınır, 429/401-403'te sıradaki anahtar denenir.
pub fn http_poster(
    settings: &VisionSettings,
    ssl: &SslVerify,
    keys: Option<Arc<dyn KeySource>>,
) -> Poster {
    let base = settings.base_url.trim_end_matches('/').to_string();
    let attempts = settings.api_keys.len().max(1);
    let keys: Arc<dyn KeySource> =
        keys.unwrap_or_else(|| Arc::new(ListKeys::new(settings.api_keys.clone())));
    let ssl = ssl.clone();
    Arc::new(move |path: &str, payload: &Value, timeout: u64| {
        if keys.acquire().is_none() {
            return Err(ApiError::new(
                "RuntimeError",
                "Kullanılabilir API anahtarı yok.",
            ));
        }
        let client = super::web_fetch::build_client(timeout, &ssl)
            .map_err(|e| ApiError::new("ConnectError", e))?;
        let body =
            serde_json::to_vec(payload).map_err(|e| ApiError::new("ValueError", e.to_string()))?;
        let url = format!("{base}{path}");
        with_key_rotation(keys.as_ref(), attempts, |key| {
            post_once(&client, &url, &body, key)
        })
    })
}

fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

// ---------------------------------------------------------------- describe_image

/// Standart base64 (dolgu ile).
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    })
}

/// `describe_image`: görseli görü modeline betimletir (metinler aynen çıkarılır).
pub fn describe_image(ctx: &ToolContext, path: &Path, display: &str) -> String {
    describe_image_with(
        path,
        display,
        &http_poster(&ctx.vision, &ctx.ssl, ctx.key_source.clone()),
    )
}

/// [`describe_image`] çekirdeği; HTTP katmanı dışarıdan verilir.
pub fn describe_image_with(path: &Path, display: &str, post: &Poster) -> String {
    let size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) => return format!("HATA: Görsel okunamadı: {e}"),
    };
    if size > MAX_IMAGE_BYTES {
        return format!(
            "HATA: Görsel çok büyük ({} KB, sınır {} KB).",
            size / 1024,
            MAX_IMAGE_BYTES / 1024
        );
    }
    let Some(mime) = image_mime(path) else {
        return format!("HATA: Desteklenmeyen görsel türü: {display}");
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return format!("HATA: Görsel okunamadı: {e}"),
    };
    let b64 = base64_encode(&bytes);
    let prompt = "Bu görseli Türkçe betimle. Görselde metin (hata mesajı, kod, log, arayüz yazısı) varsa \
                  aynen, eksiksiz çıkar. Önce kısa betimleme, sonra 'Görünen metin:' başlığı altında metinler.";
    let payload = json!({
        "model": VISION_MODEL,
        "max_tokens": 1500,
        "temperature": 0.1,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": prompt},
            {"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{b64}")}},
        ]}],
    });
    let text = match post("/chat/completions", &payload, 120).and_then(|data| {
        let msg = data
            .pointer("/choices/0/message")
            .ok_or_else(|| ApiError::new("KeyError", "'choices'"))?;
        Ok(msg
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string())
    }) {
        Ok(t) => t,
        Err(e) => return format!("HATA: Görsel betimlenemedi ({}): {e}", e.kind),
    };
    let text = if text.is_empty() {
        "(boş yanıt)".to_string()
    } else {
        text
    };
    format!("--- {display} (görsel, {VISION_MODEL} betimlemesi) ---\n{text}")
}

// ---------------------------------------------------------------- anlamsal arama

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    let mut subdirs = Vec::new();
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if ft.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                subdirs.push(p);
            }
            continue;
        }
        // os.walk: dizine giden sembolik bağ dizin listesindedir, girilmez.
        let meta = match std::fs::metadata(&p) {
            Ok(m) if !m.is_dir() => m,
            _ => continue,
        };
        let ext_ok = p
            .extension()
            .map(|x| TEXT_EXTS.contains(&x.to_string_lossy().to_lowercase().as_str()))
            .unwrap_or(false);
        if ext_ok && meta.len() <= MAX_INDEX_FILE_BYTES {
            out.push(p);
        }
    }
    for d in subdirs {
        walk(&d, out);
    }
}

/// `_iter_files`: indekslenecek metin dosyaları (os.walk sırası, ad sıralı).
pub fn iter_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn rel_posix(p: &Path, root: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn mtime(p: &Path) -> std::io::Result<f64> {
    let m = std::fs::metadata(p)?.modified()?;
    Ok(m.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0))
}

fn read_text_lossy(p: &Path) -> std::io::Result<String> {
    std::fs::read(p).map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn index_file(root: &Path) -> PathBuf {
    root.join(".evren").join("index").join("index.json")
}

fn load_index(root: &Path) -> Value {
    std::fs::read_to_string(index_file(root))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"files": {}}))
}

/// mtime karşılaştırması: JSON float ayrıştırması son bitte sapabilir
/// (serde_json `float_roundtrip` kapalı; Python ile ortak indeks), 1 µs tolerans.
fn same_mtime(entry: Option<&Value>, mtime: f64) -> bool {
    entry
        .and_then(|f| f.get("mtime"))
        .and_then(Value::as_f64)
        .is_some_and(|m| (m - mtime).abs() < 1e-6)
}

/// `_pending_chunks`: yeniden gömülecek yaklaşık parça sayısı (sınırı aşınca durur).
pub fn pending_chunks(root: &Path) -> usize {
    let index = load_index(root);
    let files = index.get("files").and_then(Value::as_object);
    let mut n = 0usize;
    for p in iter_files(root) {
        let rel = rel_posix(&p, root);
        let Ok(mt) = mtime(&p) else { continue };
        if same_mtime(files.and_then(|f| f.get(&rel)), mt) {
            continue;
        }
        let Ok(text) = read_text_lossy(&p) else {
            continue;
        };
        n += (split_lines(&text).len() / CHUNK_LINES + 1).max(1);
        if n > SYNC_CHUNK_LIMIT {
            break;
        }
    }
    n
}

/// `_embed`: 32'lik gruplarla gömme vektörleri.
fn embed(texts: &[String], post: &Poster) -> Result<Vec<Vec<f64>>, ApiError> {
    let mut out = Vec::with_capacity(texts.len());
    for batch in texts.chunks(EMBED_BATCH) {
        let data = post(
            "/embeddings",
            &json!({"model": EMBED_MODEL, "input": batch}),
            120,
        )?;
        let mut items: Vec<Value> = data
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| ApiError::new("KeyError", "'data'"))?;
        items.sort_by_key(|d| d.get("index").and_then(Value::as_i64).unwrap_or(0));
        for d in items {
            let v = d
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(|| ApiError::new("KeyError", "'embedding'"))?;
            out.push(v.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect());
        }
    }
    Ok(out)
}

/// Kosinüs benzerliği; sıfır vektörde 0.
pub fn cosine(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nb = b.iter().map(|y| y * y).sum::<f64>().sqrt();
    if na != 0.0 && nb != 0.0 {
        dot / (na * nb)
    } else {
        0.0
    }
}

struct Chunk {
    start: usize,
    end: usize,
    text: String,
}

/// `_update_index`: değişen dosyaları parçalayıp gömer, `.evren/index/index.json` yazar.
pub fn update_index(root: &Path, post: &Poster) -> Result<Value, ApiError> {
    let io = |e: std::io::Error| ApiError::new("OSError", e.to_string());
    let idx_file = index_file(root);
    if let Some(dir) = idx_file.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let mut index = load_index(root);
    if !index.get("files").is_some_and(Value::is_object) {
        index["files"] = json!({});
    }
    let mut files: Map<String, Value> = match index.get_mut("files").map(Value::take) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let mut seen = std::collections::HashSet::new();
    let mut todo: Vec<(String, f64, Vec<Chunk>)> = Vec::new();
    for p in iter_files(root) {
        let rel = rel_posix(&p, root);
        seen.insert(rel.clone());
        let mt = mtime(&p).map_err(io)?;
        if same_mtime(files.get(&rel), mt) {
            continue;
        }
        let Ok(text) = read_text_lossy(&p) else {
            continue;
        };
        let lines = split_lines(&text);
        let mut chunks = Vec::new();
        for s in (0..lines.len()).step_by(CHUNK_LINES) {
            let e = (s + CHUNK_LINES).min(lines.len());
            let body = lines[s..e].join("\n");
            if !body.trim().is_empty() {
                chunks.push(Chunk {
                    start: s + 1,
                    end: e,
                    text: body,
                });
            }
        }
        todo.push((rel, mt, chunks));
    }
    let removed: Vec<String> = files
        .keys()
        .filter(|k| !seen.contains(*k))
        .cloned()
        .collect();
    for rel in &removed {
        files.remove(rel);
    }
    let inputs: Vec<String> = todo
        .iter()
        .flat_map(|(rel, _, cs)| {
            cs.iter().map(move |c| {
                take_chars(&format!("{rel}\n{}", c.text), EMBED_INPUT_CHARS).to_string()
            })
        })
        .collect();
    let vecs = if inputs.is_empty() {
        Vec::new()
    } else {
        embed(&inputs, post)?
    };
    let mut vec_iter = vecs.into_iter();
    for (rel, mt, cs) in &todo {
        let mut stored = Vec::new();
        for c in cs {
            let Some(v) = vec_iter.next() else { break };
            stored.push(json!({
                "start": c.start,
                "end": c.end,
                "text": take_chars(&c.text, CHUNK_STORE_CHARS),
                "vec": v,
            }));
        }
        files.insert(rel.clone(), json!({"mtime": mt, "chunks": stored}));
    }
    index["files"] = Value::Object(files);
    if !todo.is_empty() || !removed.is_empty() {
        let text = serde_json::to_string(&index)
            .map_err(|e| ApiError::new("ValueError", e.to_string()))?;
        std::fs::write(&idx_file, text).map_err(io)?;
    }
    Ok(index)
}

fn building() -> &'static Mutex<HashMap<String, JoinHandle<()>>> {
    static B: OnceLock<Mutex<HashMap<String, JoinHandle<()>>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

fn root_key(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Bu kök için arka plan indeksi hâlâ çalışıyor mu?
pub fn is_building(root: &Path) -> bool {
    let map = building().lock().unwrap_or_else(|p| p.into_inner());
    map.get(&root_key(root)).is_some_and(|h| !h.is_finished())
}

/// `_start_background_index`: indeksi ayrı iş parçacığında kurar.
fn start_background_index(root: &Path, post: &Poster) {
    let key = root_key(root);
    let mut map = building().lock().unwrap_or_else(|p| p.into_inner());
    if map.get(&key).is_some_and(|h| !h.is_finished()) {
        return;
    }
    let root = root.to_path_buf();
    let post = Arc::clone(post);
    let spawned = std::thread::Builder::new()
        .name("evren-index".into())
        .spawn(move || {
            let _ = update_index(&root, &post);
        });
    if let Ok(h) = spawned {
        map.insert(key, h);
    }
}

/// `search_code` aracı: bağlamın API ayarlarıyla.
pub fn search_code(ctx: &ToolContext, query: &str) -> String {
    let post = http_poster(&ctx.vision, &ctx.ssl, ctx.key_source.clone());
    search_code_with(query, &ctx.root(), ctx.vision.semantic_search, &post)
}

/// [`search_code`] çekirdeği; HTTP katmanı dışarıdan verilir.
pub fn search_code_with(query: &str, root: &Path, enabled: bool, post: &Poster) -> String {
    if !enabled {
        return "HATA: Anlamsal arama kapalı (EVREN_SEMANTIC_SEARCH=true ile açılır).".to_string();
    }
    if query.trim().is_empty() {
        return "HATA: 'query' boş.".to_string();
    }
    if is_building(root) || pending_chunks(root) > SYNC_CHUNK_LIMIT {
        start_background_index(root, post);
        return "BİLGİ: Anlamsal indeks arka planda hazırlanıyor (ilk kullanım). Bu adımda \
                find_files(query=\"a|b\") kullan; birkaç dakika sonra search_code tekrar dene."
            .to_string();
    }
    match run_search(query, root, post) {
        Ok(s) => s,
        Err(e) => format!("HATA: Anlamsal arama başarısız ({}): {e}", e.kind),
    }
}

fn run_search(query: &str, root: &Path, post: &Poster) -> Result<String, ApiError> {
    let index = update_index(root, post)?;
    let qv = embed(&[query.to_string()], post)?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::new("IndexError", "list index out of range"))?;
    let mut scored: Vec<(f64, String, &Value)> = Vec::new();
    if let Some(files) = index.get("files").and_then(Value::as_object) {
        for (rel, f) in files {
            for c in f
                .get("chunks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let v: Vec<f64> = c
                    .get("vec")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
                    .unwrap_or_default();
                scored.push((cosine(&qv, &v), rel.clone(), c));
            }
        }
    }
    // Python `sort(reverse=True)` kararlıdır: eşitlerde ilk gelen önde kalır.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(TOP_K);
    if scored.is_empty() {
        return Ok("Sonuç yok (indekslenecek metin dosyası bulunamadı).".to_string());
    }
    let chunk_text = |c: &Value| {
        c.get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let docs: Vec<String> = scored
        .iter()
        .map(|(_, r, c)| format!("{r}\n{}", chunk_text(c)))
        .collect();
    let order = rerank(query, &docs, post).unwrap_or_else(|| (0..scored.len()).collect());
    let mut out = Vec::new();
    for &i in order.iter().take(RESULT_N) {
        let (_, rel, c) = scored
            .get(i)
            .ok_or_else(|| ApiError::new("IndexError", "list index out of range"))?;
        let text = chunk_text(c);
        let snippet = split_lines(&text)
            .into_iter()
            .take(6)
            .collect::<Vec<_>>()
            .join("\n");
        let num = |k: &str| c.get(k).and_then(Value::as_i64).unwrap_or(0);
        out.push(format!("{rel}:{}-{}\n{snippet}", num("start"), num("end")));
    }
    Ok(format!(
        "En ilgili parçalar (view_file ile ayrıntıya bak):\n\n{}",
        out.join("\n\n")
    ))
}

/// Yeniden sıralama; her hata ya da boş sonuç `None` (kosinüs sırası kalır).
fn rerank(query: &str, docs: &[String], post: &Poster) -> Option<Vec<usize>> {
    let rr = post(
        "/rerank",
        &json!({"model": RERANK_MODEL, "query": query, "documents": docs, "top_n": RESULT_N}),
        60,
    )
    .ok()?;
    let res = ["results", "data"]
        .iter()
        .filter_map(|k| rr.get(*k).and_then(Value::as_array))
        .find(|a| !a.is_empty())
        .cloned()
        .unwrap_or_default();
    let score = |x: &Value| {
        x.get("relevance_score")
            .or_else(|| x.get("score"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    let mut res = res;
    res.sort_by(|a, b| {
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut ranked = Vec::with_capacity(res.len());
    for x in &res {
        ranked.push(usize::try_from(x.get("index")?.as_u64()?).ok()?);
    }
    if ranked.is_empty() {
        None
    } else {
        Some(ranked)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fake_vec(text: &str) -> Value {
        json!([
            text.matches("alpha").count() as f64,
            text.matches("beta").count() as f64,
            0.01
        ])
    }

    /// Sahte API: gömme anahtar kelime sayımı, rerank opsiyonel ters sıra.
    fn fake_poster(rerank_reverse: bool, calls: Arc<AtomicUsize>) -> Poster {
        Arc::new(move |path: &str, payload: &Value, _t: u64| {
            calls.fetch_add(1, Ordering::SeqCst);
            match path {
                "/embeddings" => {
                    let inputs = payload["input"].as_array().unwrap();
                    let data: Vec<Value> = inputs
                        .iter()
                        .enumerate()
                        .rev()
                        .map(|(i, t)| json!({"index": i, "embedding": fake_vec(t.as_str().unwrap())}))
                        .collect();
                    Ok(json!({ "data": data }))
                }
                "/rerank" if rerank_reverse => {
                    let n = payload["documents"].as_array().unwrap().len();
                    let results: Vec<Value> = (0..n)
                        .map(|i| json!({"index": i, "relevance_score": i as f64}))
                        .collect();
                    Ok(json!({ "results": results }))
                }
                "/rerank" => Err(ApiError::new("HTTPStatusError", "HTTP 404")),
                "/chat/completions" => {
                    let url = payload["messages"][0]["content"][1]["image_url"]["url"]
                        .as_str()
                        .unwrap();
                    assert!(url.starts_with("data:image/png;base64,"));
                    assert_eq!(payload["model"], VISION_MODEL);
                    Ok(json!({"choices": [{"message": {"content": "  bir kedi  "}}]}))
                }
                _ => Err(ApiError::new("HTTPStatusError", "HTTP 404")),
            }
        })
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-12);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn settings_from_config() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("EVREN_SEMANTIC_SEARCH".to_string(), "off".to_string());
        env.insert("EVREN_API_KEY".to_string(), "evren_llm_k".to_string());
        let s = VisionSettings::from_config(&Config::from_env(env));
        assert!(!s.semantic_search);
        assert_eq!(s.api_keys, vec!["evren_llm_k"]);
        let s = VisionSettings::from_config(&Config::from_env(Default::default()));
        assert!(s.semantic_search);
    }

    #[test]
    fn iter_files_skips_dirs_ext_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "a.py", "x");
        write(r, "img.png", "x");
        write(r, "node_modules/m.js", "x");
        write(r, "dist/d.js", "x");
        write(r, "sub/b.RS", "x");
        write(r, "big.txt", &"a".repeat(300_001));
        let rels: Vec<String> = iter_files(r).iter().map(|p| rel_posix(p, r)).collect();
        assert_eq!(rels, vec!["a.py", "sub/b.RS"]);
    }

    #[test]
    fn search_disabled_and_empty_query() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let post = fake_poster(false, calls.clone());
        assert!(search_code_with("x", dir.path(), false, &post).contains("kapalı"));
        assert_eq!(
            search_code_with("  ", dir.path(), true, &post),
            "HATA: 'query' boş."
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn search_ranks_by_cosine_and_reuses_index() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "a.py", "def alpha():\n    return 1\n");
        write(r, "b.py", "def beta():\n    beta = 2\n");
        let calls = Arc::new(AtomicUsize::new(0));
        let post = fake_poster(false, calls.clone());
        let out = search_code_with("beta nerede", r, true, &post);
        assert!(out.starts_with("En ilgili parçalar"), "{out}");
        let b = out.find("b.py:1-2").unwrap();
        let a = out.find("a.py:1-2").unwrap();
        assert!(b < a, "{out}");
        assert!(out.contains("def beta():"));
        assert!(index_file(r).is_file());
        // index + sorgu gömme + rerank = 3 çağrı
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        // İkinci aramada dosyalar değişmedi: yalnız sorgu gömme + rerank.
        let _ = search_code_with("alpha", r, true, &post);
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        let idx = load_index(r);
        assert_eq!(idx["files"]["a.py"]["chunks"][0]["start"], 1);
        // Silinen dosya indeksten düşer.
        std::fs::remove_file(r.join("a.py")).unwrap();
        let out = search_code_with("alpha", r, true, &post);
        assert!(!out.contains("a.py"), "{out}");
        assert!(load_index(r)["files"].get("a.py").is_none());
    }

    #[test]
    fn rerank_order_overrides_cosine() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "a.py", "alpha alpha\n");
        write(r, "b.py", "beta\n");
        let post = fake_poster(true, Arc::new(AtomicUsize::new(0)));
        let out = search_code_with("alpha", r, true, &post);
        // Kosinüs: a önde; rerank en yüksek puanı son belgeye (b) verir.
        assert!(
            out.find("b.py").unwrap() < out.find("a.py").unwrap(),
            "{out}"
        );
    }

    #[test]
    fn search_chunks_long_files() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let body: String = (0..130).map(|i| format!("line {i}\n")).collect();
        write(r, "long.md", &body);
        let post = fake_poster(false, Arc::new(AtomicUsize::new(0)));
        let out = search_code_with("satır", r, true, &post);
        assert!(out.contains("long.md:1-60"), "{out}");
        assert!(out.contains("long.md:61-120"), "{out}");
        assert!(out.contains("long.md:121-130"), "{out}");
    }

    #[test]
    fn search_no_files_and_api_error() {
        let dir = tempfile::tempdir().unwrap();
        let post = fake_poster(false, Arc::new(AtomicUsize::new(0)));
        assert_eq!(
            search_code_with("x", dir.path(), true, &post),
            "Sonuç yok (indekslenecek metin dosyası bulunamadı)."
        );
        let failing: Poster =
            Arc::new(|_: &str, _: &Value, _: u64| Err(ApiError::new("RuntimeError", "yok")));
        assert_eq!(
            search_code_with("x", dir.path(), true, &failing),
            "HATA: Anlamsal arama başarısız (RuntimeError): yok"
        );
    }

    #[test]
    fn large_repo_goes_to_background() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for i in 0..45 {
            write(r, &format!("f{i:02}.txt"), "alpha\n");
        }
        assert!(pending_chunks(r) > SYNC_CHUNK_LIMIT);
        let post = fake_poster(false, Arc::new(AtomicUsize::new(0)));
        let out = search_code_with("alpha", r, true, &post);
        assert!(
            out.starts_with("BİLGİ: Anlamsal indeks arka planda"),
            "{out}"
        );
        // Arka plan işi bitince indeks hazırdır ve eşzamanlı arama çalışır.
        let key = root_key(r);
        let handle = building().lock().unwrap().remove(&key);
        if let Some(h) = handle {
            h.join().unwrap();
        }
        assert_eq!(pending_chunks(r), 0);
        let out = search_code_with("alpha", r, true, &post);
        assert!(out.starts_with("En ilgili parçalar"), "{out}");
    }

    #[test]
    fn describe_image_ok_too_big_and_error() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("x.png");
        std::fs::write(&img, [0x89u8, b'P', b'N', b'G']).unwrap();
        let post = fake_poster(false, Arc::new(AtomicUsize::new(0)));
        assert_eq!(
            describe_image_with(&img, "x.png", &post),
            format!("--- x.png (görsel, {VISION_MODEL} betimlemesi) ---\nbir kedi")
        );
        let empty: Poster = Arc::new(|_: &str, _: &Value, _: u64| {
            Ok(json!({"choices": [{"message": {"content": null}}]}))
        });
        assert!(describe_image_with(&img, "x.png", &empty).ends_with("(boş yanıt)"));
        let bad: Poster = Arc::new(|_: &str, _: &Value, _: u64| Ok(json!({})));
        assert_eq!(
            describe_image_with(&img, "x.png", &bad),
            "HATA: Görsel betimlenemedi (KeyError): 'choices'"
        );
        let big = dir.path().join("big.png");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        assert_eq!(
            describe_image_with(&big, "big.png", &post),
            "HATA: Görsel çok büyük (8192 KB, sınır 8192 KB)."
        );
    }

    #[test]
    fn key_rotation_skips_rate_limited_and_rejected() {
        let keys = ListKeys::new(vec!["a".into(), "b".into(), "c".into()]);
        let mut tried = Vec::new();
        let res = with_key_rotation(&keys, 3, |key| {
            tried.push(key.to_string());
            match key {
                "a" => KeyAttempt::RateLimited(Some(1.0), ApiError::new("HTTPStatusError", "429")),
                "b" => KeyAttempt::Rejected(ApiError::new("HTTPStatusError", "401")),
                _ => KeyAttempt::Done(json!({"ok": key})),
            }
        });
        assert_eq!(res.unwrap(), json!({"ok": "c"}));
        assert_eq!(tried, vec!["a", "b", "c"]);
        // Atlanan anahtarlar aynı poster içinde tekrar denenmez.
        assert_eq!(keys.acquire().as_deref(), Some("c"));
    }

    #[test]
    fn key_rotation_stops_on_key_independent_error() {
        let keys = ListKeys::new(vec!["a".into(), "b".into()]);
        let mut calls = 0;
        let err = with_key_rotation(&keys, 2, |_| {
            calls += 1;
            KeyAttempt::Failed(ApiError::new("ConnectError", "yok"))
        })
        .unwrap_err();
        assert_eq!((calls, err.kind.as_str()), (1, "ConnectError"));
        let all_bad = with_key_rotation(&keys, 2, |_| {
            KeyAttempt::Rejected(ApiError::new("HTTPStatusError", "HTTP 403"))
        })
        .unwrap_err();
        assert_eq!(all_bad.message, "HTTP 403");
    }

    #[test]
    fn http_poster_without_key_fails_offline() {
        let s = VisionSettings {
            base_url: "http://127.0.0.1:9".into(),
            api_keys: vec![],
            semantic_search: true,
        };
        let post = http_poster(&s, &SslVerify::Flag(true), None);
        let err = post("/embeddings", &json!({}), 1).unwrap_err();
        assert_eq!(err.kind, "RuntimeError");
        assert_eq!(err.message, "Kullanılabilir API anahtarı yok.");
    }
}
