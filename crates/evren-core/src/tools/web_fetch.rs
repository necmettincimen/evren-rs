//! `web_fetch` aracı (Python `web_fetch.py`): http/https GET, HTML'i okunur
//! metne çevirir, JSON/düz metni olduğu gibi döndürür. Boyut, zaman ve
//! karakter sınırlıdır; hatalar her zaman kısa "HATA: ..." metnidir.

use std::io::Read;
use std::sync::LazyLock;
use std::time::Duration;

use regex::{Captures, Regex};

use crate::config::SslVerify;

pub const DEFAULT_TIMEOUT: u64 = 15;
pub const MAX_TIMEOUT: u64 = 60;
pub const DEFAULT_MAX_BYTES: usize = 200_000;
/// Her araç sonucu geçmişte kalır; belge sayfası ~3.5k token ile sınırlı.
pub const DEFAULT_MAX_CHARS: usize = 12_000;

const USER_AGENT: &str = "Mozilla/5.0 (compatible; EVREN-CLI/2.0; +web_fetch)";
const ACCEPT: &str =
    "text/html,application/xhtml+xml,application/json;q=0.9,text/plain;q=0.8,*/*;q=0.5";

/// İçeriği tamamen atılan etiketler.
const DROP_TAGS: &[&str] = &[
    "script", "style", "nav", "header", "footer", "iframe", "noscript", "svg",
];

/// Metin bloğunu bitiren (satır sonu olan) blok etiketleri.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "aside",
    "ul",
    "ol",
    "table",
    "tr",
    "blockquote",
    "pre",
    "figure",
    "figcaption",
    "form",
    "fieldset",
    "dl",
    "dt",
    "dd",
    "address",
    "hr",
];

/// Metin olarak çözülünce bağlamı bozuk karakterle dolduran ikili türler.
const BINARY_TYPES: &[&str] = &[
    "application/pdf",
    "application/zip",
    "application/octet-stream",
    "image/",
    "audio/",
    "video/",
    "font/",
    "application/x-",
];

/// Sabit desenler derleme anında doğrulanır (testler hepsini çalıştırır).
#[allow(clippy::expect_used)]
fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("geçerli düzenli ifade")
}

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| re(r"<[^>]+>"));
static WS_RE: LazyLock<Regex> = LazyLock::new(|| re(r"[ \t\x0c\x0b]+"));
static MANY_BLANK_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\n{3,}"));
static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<title[^>]*>(.*?)</title\s*>"));
static BODY_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<body[^>]*>(.*)</body>"));
static DROP_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    DROP_TAGS
        .iter()
        .map(|t| re(&format!(r"(?is)<{t}\b[^>]*>.*?</{t}\s*>")))
        .collect()
});
static DROP_LOOSE_RE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?i)</?(?:{})\b[^>]*>", DROP_TAGS.join("|"))));
static COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<!--.*?-->"));
static LINK_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<a\b([^>]*)>(.*?)</a\s*>"));
static HEADING_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    (1..=6)
        .map(|n| re(&format!(r"(?is)<h{n}\b[^>]*>(.*?)</h{n}\s*>")))
        .collect()
});
static LI_OPEN_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<li\b[^>]*>"));
static LI_CLOSE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</li\s*>"));
static BR_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<br\s*/?>"));
static BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?i)</?(?:{})\b[^>]*>", BLOCK_TAGS.join("|"))));
static HREF_RES: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    [
        re(r#"(?i)\bhref\s*=\s*"([^"]*)""#),
        re(r"(?i)\bhref\s*=\s*'([^']*)'"),
        // Küçültülmüş HTML tırnakları atar: href=/docs/x
        re(r#"(?i)\bhref\s*=\s*([^\s"'>]+)"#),
    ]
});
static ENTITY_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"&(#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6}|[A-Za-z][A-Za-z0-9]{1,31});?"));
static CHARSET_RE: LazyLock<Regex> = LazyLock::new(|| re(r"charset=([\w\-]+)"));

/// URL kullanılabilir bir http(s) adresi değilse hata metni.
pub fn validate_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return Some("HATA: web_fetch için boş URL.".to_string());
    }
    let lowered = url.to_lowercase();
    if !(lowered.starts_with("http://") || lowered.starts_with("https://")) {
        return Some("HATA: web_fetch yalnızca http/https URL'lerini destekler.".to_string());
    }
    None
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" | "AMP" => "&",
        "lt" | "LT" => "<",
        "gt" | "GT" => ">",
        "quot" | "QUOT" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "hellip" => "…",
        "ndash" => "–",
        "mdash" => "—",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "laquo" => "«",
        "raquo" => "»",
        "bull" => "•",
        "middot" => "·",
        "euro" => "€",
        "deg" => "°",
        "times" => "×",
        _ => return None,
    })
}

/// `html.unescape` karşılığı: sayısal ve yaygın adlı varlıklar.
pub fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    ENTITY_RE
        .replace_all(text, |c: &Captures| {
            let body = &c[1];
            let decoded = if let Some(num) = body.strip_prefix('#') {
                let code = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse::<u32>().ok(),
                };
                code.map(|n| char::from_u32(n).unwrap_or('\u{fffd}').to_string())
            } else {
                named_entity(body).map(str::to_string)
            };
            decoded.unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

fn href(attrs: &str) -> String {
    HREF_RES
        .iter()
        .find_map(|r| r.captures(attrs).map(|c| unescape(&c[1])))
        .unwrap_or_default()
}

fn strip_tags(text: &str) -> String {
    TAG_RE.replace_all(text, "").into_owned()
}

/// HTML belgesini okunur düz metne çevirir (`_html_to_text`).
pub fn html_to_text(text: &str) -> String {
    // Sayfa başlığı gövde çıkarımında atılan <head> içindedir.
    let title = TITLE_RE
        .captures(text)
        .map(|c| unescape(&strip_tags(&c[1])).trim().to_string())
        .unwrap_or_default();

    let mut text = match BODY_RE.captures(text) {
        Some(c) => c[1].to_string(),
        None => text.to_string(),
    };
    for r in DROP_RES.iter() {
        text = r.replace_all(&text, "").into_owned();
    }
    text = DROP_LOOSE_RE.replace_all(&text, "").into_owned();
    text = COMMENT_RE.replace_all(&text, "").into_owned();

    text = LINK_RE
        .replace_all(&text, |c: &Captures| {
            let inner = strip_tags(&c[2]).trim().to_string();
            let href = href(&c[1]).trim().to_string();
            if inner.is_empty() {
                String::new()
            } else if href.is_empty() {
                inner
            } else {
                format!("{inner} ({href})")
            }
        })
        .into_owned();

    for (i, r) in HEADING_RES.iter().enumerate() {
        let hashes = "#".repeat(i + 1);
        text = r
            .replace_all(&text, |c: &Captures| {
                let inner = strip_tags(&c[1]).trim().to_string();
                if inner.is_empty() {
                    String::new()
                } else {
                    format!("\n{hashes} {inner}\n")
                }
            })
            .into_owned();
    }

    text = LI_OPEN_RE.replace_all(&text, "\n- ").into_owned();
    text = LI_CLOSE_RE.replace_all(&text, "\n").into_owned();
    text = BR_RE.replace_all(&text, "\n").into_owned();
    text = BLOCK_RE.replace_all(&text, "\n").into_owned();
    text = unescape(&strip_tags(&text));

    let lines: Vec<String> = text
        .lines()
        .map(|l| WS_RE.replace_all(l, " ").trim().to_string())
        .collect();
    let text = lines.join("\n");
    let text = MANY_BLANK_RE.replace_all(&text, "\n\n").trim().to_string();
    if !title.is_empty() && !text.starts_with(&format!("# {title}")) {
        return if text.is_empty() {
            format!("# {title}")
        } else {
            format!("# {title}\n\n{text}")
        };
    }
    text
}

/// Baştan `max_chars` karakter tutar, atılan miktarı not eder.
pub fn truncate(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if max_chars == 0 || total <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}\n... [{} karakter kırpıldı] ...", total - max_chars)
}

fn decode(body: &[u8], charset: &str) -> String {
    match charset.to_lowercase().as_str() {
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" | "cp1252" | "us-ascii" | "ascii" => {
            body.iter().map(|&b| b as char).collect()
        }
        _ => String::from_utf8_lossy(body).into_owned(),
    }
}

fn first_chars_lower(text: &str, n: usize) -> String {
    text.chars().take(n).collect::<String>().to_lowercase()
}

fn build_client(timeout: u64, ssl: &SslVerify) -> Result<reqwest::blocking::Client, String> {
    let mut b = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .user_agent(USER_AGENT);
    match ssl {
        SslVerify::Flag(false) => b = b.danger_accept_invalid_certs(true),
        SslVerify::Flag(true) => {}
        SslVerify::CaBundle(path) => {
            let pem = std::fs::read(path).map_err(|e| e.to_string())?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| e.to_string())?;
            for c in certs {
                b = b.add_root_certificate(c);
            }
        }
    }
    b.build().map_err(|e| e.to_string())
}

/// Hatanın kök nedeni (reqwest katmanlarının en içteki açıklaması).
fn root_cause(err: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = err;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

/// `url`'yi GET eder; okunur metin döndürür, asla paniklemez (`fetch_url`).
pub fn fetch_url(
    url: &str,
    timeout: Option<i64>,
    max_bytes: Option<i64>,
    max_chars: Option<i64>,
    ssl: &SslVerify,
) -> String {
    if let Some(err) = validate_url(url) {
        return err;
    }
    let url = url.trim();
    let timeout = timeout
        .filter(|&t| t > 0)
        .map(|t| t as u64)
        .unwrap_or(DEFAULT_TIMEOUT)
        .min(MAX_TIMEOUT);
    let max_bytes = max_bytes
        .filter(|&b| b > 0)
        .map(|b| b as usize)
        .unwrap_or(DEFAULT_MAX_BYTES);
    let max_chars = max_chars
        .filter(|&c| c > 0)
        .map(|c| c as usize)
        .unwrap_or(DEFAULT_MAX_CHARS);

    let client = match build_client(timeout, ssl) {
        Ok(c) => c,
        Err(e) => return format!("HATA: Beklenmeyen hata: {e} ({url})"),
    };
    let resp = match client.get(url).header("Accept", ACCEPT).send() {
        Ok(r) => r,
        Err(e) if e.is_timeout() => return format!("HATA: Zaman aşımı ({timeout}s) ({url})"),
        Err(e) => return format!("HATA: Bağlantı kurulamadı: {} ({url})", root_cause(&e)),
    };
    let status = resp.status();
    if !status.is_success() {
        let reason = status.canonical_reason().unwrap_or("");
        return format!("HATA: HTTP {} {reason} ({url})", status.as_u16());
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if BINARY_TYPES.iter().any(|k| content_type.starts_with(k)) {
        let kind = content_type.split(';').next().unwrap_or("");
        return format!(
            "HATA: İkili içerik ({kind}) metne çevrilemez ({url}). Dosya gerekiyorsa run_command ile indir."
        );
    }
    let mut body = Vec::new();
    if let Err(e) = resp.take(max_bytes as u64 + 1).read_to_end(&mut body) {
        let cause = root_cause(&e);
        if cause.to_lowercase().contains("timed out") {
            return format!("HATA: Zaman aşımı ({timeout}s) ({url})");
        }
        return format!("HATA: Beklenmeyen hata: {cause} ({url})");
    }
    let mut size_note = String::new();
    if body.len() > max_bytes {
        body.truncate(max_bytes);
        size_note = format!("\n\n[UYARI: Gövde {max_bytes} bayt sınırında kesildi.]");
    }

    let charset = CHARSET_RE
        .captures(&content_type)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "utf-8".to_string());
    let text = decode(&body, &charset);

    let result = if content_type.contains("json") || content_type.contains("text/plain") {
        text
    } else {
        let head = first_chars_lower(&text, 2000);
        if content_type.contains("html") || head.contains("<html") || head.contains("<body") {
            html_to_text(&text)
        } else {
            // Bilinmeyen içerik: ham baytı dökmek yerine etiketleri sil.
            strip_tags(&text)
        }
    };
    truncate(&result, max_chars) + &size_note
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    const HTML: &str = r#"<html><head><style>.a{color:red}</style><script>alert(1)</script></head>
<body><nav><a href="/x">Menu</a></nav>
<h1>Başlık</h1><p>Merhaba <b>dünya</b>.</p>
<ul><li>öğe bir</li><li>öğe iki</li></ul>
<a href="https://example.com/a">bağlantı</a>
<div>blok sonu</div></body></html>"#;

    /// Tek isteğe verilen ham yanıtı döndüren yerel sunucu.
    fn serve_once(status: &str, content_type: &str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("yerel soket");
        let addr = listener.local_addr().expect("adres");
        let status = status.to_string();
        let content_type = content_type.to_string();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}/")
    }

    fn fetch(url: &str) -> String {
        fetch_url(url, None, None, None, &SslVerify::Flag(true))
    }

    #[test]
    fn rejects_non_http_scheme() {
        let r = fetch("ftp://example.com/file");
        assert!(r.starts_with("HATA") && r.contains("http"), "{r}");
        assert_eq!(
            validate_url("  "),
            Some("HATA: web_fetch için boş URL.".into())
        );
        assert!(validate_url("HTTPS://x").is_none());
    }

    #[test]
    fn html_conversion() {
        let out = html_to_text(HTML);
        assert!(out.contains("# Başlık"), "{out}");
        assert!(
            !out.contains("alert") && !out.contains("color:red"),
            "{out}"
        );
        assert!(!out.contains("Menu"), "{out}");
        assert!(out.contains("Merhaba dünya."), "{out}");
        assert!(
            out.contains("- öğe bir") && out.contains("- öğe iki"),
            "{out}"
        );
        assert!(out.contains("bağlantı (https://example.com/a)"), "{out}");
        assert!(out.contains("blok sonu"), "{out}");
        assert!(!out.contains('<'), "{out}");
    }

    #[test]
    fn html_title_entities_and_unquoted_href() {
        let doc = "<html><head><title>Ana &amp; Sayfa</title></head><body>\
                   <p>a&nbsp;&lt;b&gt; &#252; &#x00e7;</p><a href=/docs/x>dok</a></body></html>";
        let out = html_to_text(doc);
        assert!(out.starts_with("# Ana & Sayfa\n\n"), "{out}");
        assert!(out.contains("<b> ü ç"), "{out}");
        assert!(out.contains("dok (/docs/x)"), "{out}");
        assert_eq!(unescape("&bilinmez; &amp"), "&bilinmez; &");
    }

    #[test]
    fn truncate_notes_dropped() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abc", 0), "abc");
        let t = truncate("çççççç", 2);
        assert!(
            t.starts_with("çç\n") && t.contains("[4 karakter kırpıldı]"),
            "{t}"
        );
    }

    #[test]
    fn fetch_html_converts() {
        let url = serve_once(
            "200 OK",
            "text/html; charset=utf-8",
            HTML.as_bytes().to_vec(),
        );
        let out = fetch(&url);
        assert!(out.contains("# Başlık"), "{out}");
        assert!(!out.contains("alert"), "{out}");
    }

    #[test]
    fn fetch_json_verbatim() {
        let url = serve_once("200 OK", "application/json", br#"{"a": 1}"#.to_vec());
        assert_eq!(fetch(&url), r#"{"a": 1}"#);
    }

    #[test]
    fn max_bytes_caps_body() {
        let url = serve_once("200 OK", "text/plain", vec![b'x'; 50_000]);
        let out = fetch_url(&url, None, Some(1000), None, &SslVerify::Flag(true));
        assert!(out.contains("kesildi"), "{out}");
        assert!(out.len() < 5000);
    }

    #[test]
    fn http_error_captured() {
        let url = serve_once("404 Not Found", "text/html", b"yok".to_vec());
        let out = fetch(&url);
        assert!(out.contains("HTTP 404"), "{out}");
        assert!(out.starts_with("HATA:"), "{out}");
    }

    #[test]
    fn binary_rejected() {
        let url = serve_once("200 OK", "application/pdf", b"%PDF".to_vec());
        let out = fetch(&url);
        assert!(out.contains("İkili içerik (application/pdf)"), "{out}");
    }

    #[test]
    fn connection_error_is_text() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("yerel soket");
        let addr = listener.local_addr().expect("adres");
        drop(listener);
        let out = fetch(&format!("http://{addr}/"));
        assert!(out.starts_with("HATA: Bağlantı kurulamadı"), "{out}");
    }
}
