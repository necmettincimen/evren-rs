//! plan `agent` ve `parallel` adımları (Python `plan_agent.py`): sonuç
//! sözleşmesi, soru yakalama, tam çıktı günlüğü, git worktree yalıtımı,
//! canlı ilerleme.
//!
//! Sözleşme (agent adımı): adım yalnız şu koşullar sağlanınca başarılı
//! sayılır; sağlanmazsa `retries` kez (varsayılan 1) eksiğe özel uyarıyla
//! yeniden denenir, yine sağlanmazsa HATA verir:
//!   - `require_changes` (varsayılan true): git deposunda dosya değişmiş olmalı;
//!   - `commit` (varsayılan false): HEAD ilerlemiş olmalı (worktree kipinde kalan değişiklikleri Evren kendisi commit eder);
//!   - `report` (yol): ajan bu dosyayı yazmış ve boş olmamalı (salt rapor/keşif adımları için `require_changes: false` ile);
//!   - ajanın son sözü soru ile bitmemeli ("onay ister misin?" gibi) — soru sorarak biten çıktı eksik sayılır.
//!
//! Ajanın TÜM çıktısı `reports/agent-logs/<etiket>-a<deneme>.log` dosyasına
//! yazılır; ekranda yalnız son satır + yol görünür. `push: true`: iş ana
//! ağaca alınınca Evren hemen push eder (Drone sonucunu bekler; pushlar
//! sıralı) — `--allow-push` gerekir. `worktree: true` (repo != kök): iş
//! `.evren/worktrees/<etiket>` altındaki geçici git worktree'de ve
//! `evren/<etiket>` dalında yapılır, bitince ana ağaca ff-only/cherry-pick
//! ile alınır; yarım kalan iş ana ağacı kirletmez. `parallel` adımı alt
//! adımları eşzamanlı koşturur (agent alt adımlarında worktree varsayılan açık).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use evren_core::tools::proc::spawn_in_group;
use regex::Regex;
use serde_json::Value;

use super::remote::expand;
use super::{
    first_chars, flag, int_or, last_chars, lock, py_str, run_capture, say, shell_command, str_of,
    tail, Step,
};

static MERGE_LOCK: Mutex<()> = Mutex::new(());
const PROGRESS_EVERY: Duration = Duration::from_secs(30);

const HEADLESS_DIRECTIVE: &str = concat!(
    "\n\n[BAŞSIZ KİP] Yalnız bu dar görevi uygula; plan sunma, soru sorma, onay isteme. ",
    "Belirsizlikte en makul varsayımla ilerle ve varsayımı raporda yaz. ",
    "Analiz ya da rapor yazıp bitirme: bulduğun her yeri HEMEN edit_file/write_file ile uygula. ",
    "Bağımsız okuma ve aramaları AYNI turda birden çok araç çağrısıyla yap ",
    "(view_file + search_code + find_files birlikte); dosyaları tek tek sırayla okuma. ",
    "Bitirmeden önce derlemeyi/çalıştırmayı tek komutla doğrula. ",
    "Büyük dosyayı TEK write_file çağrısında yazma (çıktı sınırında kesilir ve iş döngüye girer): ",
    "çağrı başına en çok ~2500 karakter; ilk parça write_file, devamı write_file(append=true). ",
    "HIZ KURALLARI (ölçüldü: bunlar ajanın süresinin çoğunu yiyordu): (1) Arama/okumayı ASLA çalışma alanı kökünde ya da ",
    "/d/ayssoftorg gibi üst dizinlerde yapma (bin/obj/node_modules dahil dakikalar sürer, zaman aşımına düşer); yalnız ",
    "ilgili repo/klasörde ve tercihen `git grep -n <desen> -- '*.cs'` (yalnız izlenen dosyalar, saniyeler) ya da ",
    "`grep -rn --include=*.cs --exclude-dir={bin,obj,node_modules,.git}` kullan. (2) Derlemeyi tüm solution yerine yalnız ",
    "değişen csproj için yap: `dotnet build <Proje>.csproj -v q --nologo`; solution derlemesi yalnız en sonda gerekiyorsa. ",
    "(3) Tüm test paketini ASLA çalıştırma; gerekirse `dotnet test --filter` ile yalnız ilgili testi. ",
    "(4) Aynı dosyayı tekrar tekrar okuma; bulgularını ilk okumada not et. (5) Gereksiz doğrulama turu atma: derleme geçtiyse commit et ve bitir."
);
const NOTE_NO_CHANGE: &str = concat!(
    "\n\n[UYARI] Önceki deneme hiçbir dosyayı değiştirmedi. Bu kez analiz yazmadan, ",
    "ilk turda ilgili dosyaları birlikte okuyup ikinci turda edit_file ile değişiklikleri uygula."
);
const NOTE_NO_COMMIT: &str = concat!(
    "\n\n[UYARI] Önceki deneme değişiklikleri commit etmedi. İşi bitirince yalnız dokunduğun dosyaları ",
    "git add ile ekleyip commit at (git add -A kullanma)."
);
const NOTE_NO_REPORT: &str =
    "\n\n[UYARI] Önceki deneme istenen rapor dosyasını yazmadı. İşi bitirmeden önce raporu yaz.";
const NOTE_REPORT_ONLY: &str = concat!(
    "\n\n[UYARI] Kod işi önceki denemede yapılıp commit edildi; TEKRAR YAPMA, koda dokunma. ",
    "Yalnız `git log -3 --stat` ve `git show` ile yapılanı gör ve istenen rapor dosyasını eksiksiz yaz, sonra bitir."
);
const REPORT_ONLY_MISSING: [&str; 3] = [
    "rapor yok",
    "rapor sonuç satırı yok",
    "VERİ YOLU bölümü yok",
];
const NOTE_STEP_LIMIT: &str = concat!(
    "\n\n[UYARI] Önceki deneme adım sınırına takılıp yarıda kaldı. Çalışma kopyasında yaptığın iş duruyor ",
    "(git status / git log). Kaldığın yerden DEVAM ET, keşfi tekrarlama; işi küçük tut, derle, commit et ve raporu yaz."
);
const NOTE_NO_OUTCOME: &str = concat!(
    "\n\n[UYARI] Raporun İLK satırı `SONUÇ: ...` olmalı (DÜZELTİLDİ | ZATEN ÇÖZÜLMÜŞ | FRONTEND | ",
    "KÜTÜPHANE DEĞİŞİKLİĞİ GEREKLİ | BELİRSİZ). Raporu bu satırla düzelt."
);
// Uçtan uca iz kuralı (3.9.2): ajan her seferinde tek katmanı düzeltip yalnız o
// katmanı test ediyordu; konumu taşıyan ara katman veriyi atıyordu.
const TRACE_DIRECTIVE: &str = concat!(
    "\n\n[UÇTAN UCA İZ] Bir okuma/yazma/hesaplama hatasını düzeltiyorsan veriyi kullanıcının çağırdığı uç noktadan ",
    "son depolama/çıktıya kadar HALKA HALKA izle (endpoint -> handler -> store/locator/mapper -> depolama/SQL/şablon) ",
    "ve değiştirdiğin değerin HER halkadan geçtiğini kodda doğrula; bir halka değeri atıyor/eziyorsa onu da düzelt. ",
    "Koddaki varsayılan yapılandırma değerlerine (kova adı, önek, URL, bayrak) güvenme: canlı değer farklı olabilir; ",
    "çözümün varsayılandan farklı ayarla da çalışmasını sağla ve testi varsayılandan FARKLI ayarla yaz. ",
    "Rapora `## VERİ YOLU` bölümü yaz: her halka bir satır, `dosya:satır — ne yapıyor — değer burada nasıl taşınıyor`; ",
    "ayarlara bağlı halkalarda `CANLI AYAR VARSAYIMI:` ile hangi ayara dayandığını yaz."
);
const NOTE_NO_TRACE: &str = concat!(
    "\n\n[UYARI] Rapor `## VERİ YOLU` bölümünü (uç noktadan depolamaya her halka dosya:satır, en az 3 halka) ",
    "içermiyor. Yolu kodda izle; değeri atan/ezen bir ara halka bulursan onu da düzelt, sonra bölümü yaz."
);
const NOTE_NO_EVIDENCE: &str = concat!(
    "\n\n[UYARI] Doğrulama raporunda dolu bir `## KANIT` bölümü olmalı: test ortamındaki istek/yanıt, ",
    "kayıt ya da log çıktısını olduğu gibi yaz. Derleme geçti demek kanıt değildir."
);
const NOTE_QUESTION: &str = concat!(
    "\n\n[UYARI] Önceki deneme soru/onay isteyerek bitti. SORU SORMA: kimse cevap vermeyecek. ",
    "Belirsizlikte en makul varsayımı seç, işi sonuna kadar yap ve varsayımı raporda belirt."
);
const OUTCOME_DIRECTIVE: &str = concat!(
    " Raporun İLK satırı sonuç satırı olsun: `SONUÇ: DÜZELTİLDİ` (kod değişti ve commit edildi) | ",
    "`SONUÇ: ZATEN ÇÖZÜLMÜŞ` (kod değişikliği gerekmiyor, hangi commit) | `SONUÇ: FRONTEND` (kaynak frontend; ",
    "beklenen API davranışını yaz) | `SONUÇ: KÜTÜPHANE DEĞİŞİKLİĞİ GEREKLİ` | `SONUÇ: BELİRSİZ` (nedenini yaz) | `SONUÇ: KISMİ` (kod değişti, kalan iş `KALAN` bölümünde). ",
    "Kod değişikliği gerekmeyen işlerde değişiklik yapmak ZORUNDA DEĞİLSİN; doğru sonucu ve gerekçesini raporla."
);
/// Kod değişikliği gerektirmeyen GEÇERLİ sonuçlar: değişiklik/commit
/// sözleşmesinden muaf, rapor şartı korunur.
pub const NO_CHANGE_OUTCOMES: [&str; 6] = ["done", "front", "lib", "unk", "passed", "failed"];
const OUTCOME_KEYS: [&str; 8] = [
    "fixed", "done", "front", "lib", "unk", "partial", "passed", "failed",
];
const FINDINGS_MAX: usize = 3500;

struct Patterns {
    trace: Regex,
    file_ref: Regex,
    heading: Regex,
    skeleton: Regex,
    outcome: Regex,
    evidence: fancy_regex::Regex,
    question: Regex,
    findings: Regex,
}

fn patterns() -> Option<&'static Patterns> {
    static P: OnceLock<Option<Patterns>> = OnceLock::new();
    P.get_or_init(|| {
        Some(Patterns {
            trace: Regex::new(r"(?im)^#+\s*VER[İIiı]\s*YOLU").ok()?,
            file_ref: Regex::new(r"[\w./-]+\.(?:cs|ts|tsx|js|jsx|py|sql|xslt|java|go|json|ya?ml):\d+")
                .ok()?,
            heading: Regex::new(r"(?m)^#+\s").ok()?,
            skeleton: Regex::new(
                r"(?i)iskelet|çalışma sürüyor|calisma suruyor|görev sonunda güncellenecek|TODO\b",
            )
            .ok()?,
            // `.` işaretli yerler Türkçe i/İ/ı büyük-küçük harf farkına toleranslıdır.
            outcome: Regex::new(concat!(
                r"(?im)^[\s>*_#`-]*SONU[ÇçCc]\s*[:：]\s*[*_`\s]*(?:",
                r"(?P<fixed>D[ÜüUu]ZELT.LD.)|",
                r"(?P<done>ZATEN\s+[ÇçCc][ÖöOo]Z[ÜüUu]LM[ÜüUu][ŞşSs])|",
                r"(?P<front>FRONTEND)|",
                r"(?P<lib>K[ÜüUu]T[ÜüUu]PHANE)|",
                r"(?P<unk>BEL.RS.Z)|",
                r"(?P<partial>K.SM.)|",
                r"(?P<passed>GE[ÇçCc]T.)|",
                r"(?P<failed>KALDI))"
            ))
            .ok()?,
            // KANIT bölümü: başlık + en az bir dolu satır (istek/yanıt, kayıt, log).
            evidence: fancy_regex::Regex::new(r"(?im)^#+\s*KANIT\b[^\n]*\n+(?!\s*(?:#|$))\s*\S").ok()?,
            question: Regex::new(concat!(
                r"(?i)(devam edeyim mi|yazayım mı|geçeyim mi|başlayayım mı|onay(ın|ınla)? .*\b(mı|mi|mu|mü)\b|hangisiyle|",
                r"should i (proceed|continue)|do you want|would you like)"
            ))
            .ok()?,
            findings: Regex::new(
                r"(?im)^\W*(yap[ıiIİ]lan|kök neden|kalan [iİıI][şs]|tek somut sonraki ad[ıiIİ]m)\b",
            )
            .ok()?,
        })
    })
    .as_ref()
}

fn read_lossy(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Yarım bırakılmış iskelet rapor mu (ör. 'SONUÇ: BELİRSİZ' + 'çalışma sürüyor')?
pub fn is_skeleton_report(path: &Path) -> bool {
    let (Some(text), Some(p)) = (read_lossy(path), patterns()) else {
        return false;
    };
    p.skeleton.is_match(&text) || text.trim().chars().count() < 200
}

/// Raporda `## VERİ YOLU` başlığı ve altında en az 3 `dosya:satır` başvurusu var mı.
pub fn report_has_trace(path: &Path) -> bool {
    let (Some(text), Some(p)) = (read_lossy(path), patterns()) else {
        return false;
    };
    let Some(m) = p.trace.find(&text) else {
        return false;
    };
    let rest = &text[m.end()..];
    let section = match p.heading.find(rest) {
        Some(h) => &rest[..h.start()],
        None => rest,
    };
    let refs: BTreeSet<&str> = p.file_ref.find_iter(section).map(|m| m.as_str()).collect();
    refs.len() >= 3
}

pub fn report_has_evidence(path: &Path) -> bool {
    let (Some(text), Some(p)) = (read_lossy(path), patterns()) else {
        return false;
    };
    p.evidence.is_match(&text).unwrap_or(false)
}

/// Kart "test"e ancak KANIT'lı ve SONUÇ'u GEÇTİ / ZATEN ÇÖZÜLMÜŞ olan raporla geçer.
#[allow(dead_code)] // test kapısı: ClickUp akışı (Faz 4 sonrası) kullanır
pub fn report_ready_for_test(path: &Path) -> bool {
    matches!(report_outcome(path), Some("passed" | "done")) && report_has_evidence(path)
}

/// Raporun `SONUÇ:` satırından sonuç anahtarı; satır yoksa `None`.
pub fn report_outcome(path: &Path) -> Option<&'static str> {
    let text = read_lossy(path)?;
    let caps = patterns()?.outcome.captures(&text)?;
    OUTCOME_KEYS.into_iter().find(|k| caps.name(k).is_some())
}

/// Çıktının son sözü soru/onay isteği mi? (yalnız kuyruk bakılır).
pub fn asks_question(text: &str) -> bool {
    let tail = last_chars(text.trim(), 350);
    if tail.is_empty() {
        return false;
    }
    tail.trim_end_matches(['*', '_', '`', ' ', '\n'])
        .ends_with('?')
        || patterns().is_some_and(|p| p.question.is_match(tail))
}

/// Önceki denemenin kendi devir notu (Yapılan / Kök neden / Kalan iş / sonraki adım).
pub fn previous_findings(text: &str) -> String {
    let body = text.trim();
    if body.is_empty() {
        return String::new();
    }
    let n = body.chars().count();
    let tail_start = body
        .char_indices()
        .nth(n.saturating_sub(6000))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let start = patterns().and_then(|p| {
        p.findings
            .find_iter(body)
            .map(|m| m.start())
            .find(|s| *s >= tail_start)
    });
    let chunk = match start {
        Some(s) => &body[s..],
        None => last_chars(body, FINDINGS_MAX),
    };
    let chunk = last_chars(chunk, FINDINGS_MAX).trim();
    format!(
        "\n\nÖNCEKİ DENEMENİN BULGULARI (doğrulanmış kabul et; keşfi TEKRARLAMA, en fazla 3 okumayla \
         doğrula ve 'sonraki adım'ı hemen edit_file ile uygula):\n{chunk}"
    )
}

pub fn slug(label: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in label.chars() {
        if c.is_ascii_alphanumeric() || "._-".contains(c) {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let s = out.trim_matches('-');
    if s.is_empty() {
        "agent".into()
    } else {
        s.to_owned()
    }
}

/// Ajan sürecinin adım başına ortamı: `env` haritası ve `planner: false`
/// (EVREN_AUTO_ROUTE=0; salt okuyan planlayıcı dar işlerde yalnız adım yer).
pub fn agent_env(step: &Step) -> Option<BTreeMap<String, String>> {
    let mut env: BTreeMap<String, String> = step
        .get("env")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), py_str(v))).collect())
        .unwrap_or_default();
    if step.get("planner") == Some(&Value::Bool(false)) {
        env.insert("EVREN_AUTO_ROUTE".into(), "0".into());
    }
    (!env.is_empty()).then_some(env)
}

/// Ajanı çalıştıran taraf; testte sahte çalıştırıcıyla değiştirilir.
pub trait AgentRunner: Send + Sync {
    /// Ajanı çalıştırır, tüm çıktıyı `log` dosyasına yazar; (çıkış kodu, günlük metni).
    fn run_once(
        &self,
        cmd: &[String],
        log: &Path,
        label: &str,
        timeout: u64,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<(i32, String), String>;
}

/// Gerçek alt süreç: çıktı dosyaya, 30 sn'de bir canlı ilerleme satırı.
pub struct ProcessRunner;

impl AgentRunner for ProcessRunner {
    fn run_once(
        &self,
        cmd: &[String],
        log: &Path,
        label: &str,
        timeout: u64,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<(i32, String), String> {
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let fh = File::create(log).map_err(|e| e.to_string())?;
        let fh2 = fh.try_clone().map_err(|e| e.to_string())?;
        let (prog, args) = cmd.split_first().ok_or("boş komut")?;
        let mut c = Command::new(prog);
        c.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(fh))
            .stderr(Stdio::from(fh2));
        if let Some(env) = env {
            c.envs(env);
        }
        let mut child = spawn_in_group(&mut c).map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        let mut last_tick = t0;
        let code = loop {
            if let Some(code) = child.wait_timeout(Duration::from_secs(2)) {
                break code;
            }
            let now = Instant::now();
            if now.duration_since(t0).as_secs() > timeout {
                child.kill_tree();
                return Err(format!(
                    "ajan {timeout} sn zaman aşımı ({label}); günlük: {}",
                    log.display()
                ));
            }
            if now.duration_since(last_tick) >= PROGRESS_EVERY {
                last_tick = now;
                let text = read_lossy(log).unwrap_or_default();
                let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                say(&format!(
                    "  … {label} çalışıyor {} sn | {} satır | {}",
                    now.duration_since(t0).as_secs(),
                    lines.len(),
                    first_chars(lines.last().copied().unwrap_or(""), 100)
                ));
            }
        };
        Ok((code, read_lossy(log).unwrap_or_default()))
    }
}

/// `push: true` için çağrılan kilitli push (Python `PUSH_HOOK`).
pub type PushHook = Box<dyn Fn(&Step) -> Result<String, String> + Send + Sync>;

/// Ajan adımlarının çalışma ortamı.
pub struct Runtime {
    /// Ajan komutunun başı (`evren` yürütülebilir dosyası).
    pub exe: Vec<String>,
    pub runner: Box<dyn AgentRunner>,
    pub push_hook: Option<PushHook>,
    /// Süreçler arası ajan slotlarının dizini (`~/.evren/slots`).
    pub slots_dir: PathBuf,
}

struct GitOut {
    code: i32,
    out: Vec<u8>,
    err: Vec<u8>,
}

impl GitOut {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out).trim().to_owned()
    }
    fn combined(&self) -> String {
        let mut all = self.out.clone();
        all.extend_from_slice(&self.err);
        String::from_utf8_lossy(&all).trim().to_owned()
    }
}

fn git_t(repo: &Path, args: &[&str], secs: u64) -> GitOut {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(args);
    match run_capture(&mut c, None, Duration::from_secs(secs)) {
        Ok(r) => GitOut {
            code: r.code,
            out: r.stdout,
            err: r.stderr,
        },
        Err(e) => GitOut {
            code: -1,
            out: Vec::new(),
            err: e.into_bytes(),
        },
    }
}

fn git(repo: &Path, args: &[&str]) -> GitOut {
    git_t(repo, args, 120)
}

fn is_git(path: &Path) -> bool {
    path.is_dir() && git(path, &["rev-parse", "--git-dir"]).code == 0
}

fn head(repo: &Path) -> String {
    git(repo, &["rev-parse", "HEAD"]).stdout()
}

/// HEAD + çalışma ağacı özeti; git deposu değilse `None`.
fn state(repo: &Path) -> Option<String> {
    let st = git_t(repo, &["status", "--porcelain"], 60);
    if st.code != 0 {
        return None;
    }
    Some(format!(
        "{}\n{}",
        head(repo),
        String::from_utf8_lossy(&st.out)
    ))
}

fn dirty(repo: &Path) -> bool {
    !String::from_utf8_lossy(&git(repo, &["status", "--porcelain"]).out)
        .trim()
        .is_empty()
}

fn fwd(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// Evren'in yardımcı dosyaları (agent.md, changelog.md, .evren/) worktree commit'ine girmesin.
fn exclude_artifacts(repo: &Path) {
    let out = git(repo, &["rev-parse", "--git-path", "info/exclude"]).stdout();
    if out.is_empty() {
        return;
    }
    let p = PathBuf::from(&out);
    let path = if p.is_absolute() { p } else { repo.join(p) };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let have = read_lossy(&path).unwrap_or_default();
    let have: Vec<&str> = have.lines().collect();
    let add: Vec<&str> = [".evren/", "/agent.md", "/changelog.md", "/.evren-rapor.md"]
        .into_iter()
        .filter(|x| !have.contains(x))
        .collect();
    if !add.is_empty() {
        use std::io::Write as _;
        if let Ok(mut fh) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = fh.write_all(format!("{}\n", add.join("\n")).as_bytes());
        }
    }
}

/// Ajan ana ağaca yazdıysa değişiklikler adlandırılmış stash'e alınır.
fn quarantine_main(repo: &Path, label: &str) -> String {
    if !dirty(repo) {
        return String::new();
    }
    let msg = format!("evren-ana-agac-kirlendi {label}");
    if git(repo, &["stash", "push", "-u", "-m", &msg]).code == 0 {
        format!(" [UYARI: ana ağaç kirlenmişti, değişiklikler stash'e alındı: evren-ana-agac-kirlendi {label}]")
    } else {
        String::new()
    }
}

/// worktree dalını ana ağaca alır: önce ff-only, olmazsa cherry-pick.
fn merge_back(repo: &Path, branch: &str, base: &str) -> Result<String, String> {
    let _g = lock(&MERGE_LOCK);
    let warn = quarantine_main(repo, branch);
    if git(repo, &["merge", "--ff-only", branch]).code == 0 {
        return Ok(format!("ff{warn}"));
    }
    let r = git(repo, &["cherry-pick", &format!("{base}..{branch}")]);
    if r.code == 0 {
        return Ok(format!("cherry-pick{warn}"));
    }
    git(repo, &["cherry-pick", "--abort"]);
    Err(format!(
        "ana ağaca alınamadı ({branch}); dal ve worktree incelenmek üzere bırakıldı: {}",
        last_chars(&r.combined(), 250)
    ))
}

fn move_file(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst).map_err(|e| e.to_string())?;
    std::fs::remove_file(src).map_err(|e| e.to_string())
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn nonempty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn run_verify(step: &Step, cwd: &Path) -> Result<(bool, String), String> {
    let verify = str_of(step, "verify").unwrap_or_default();
    let mut c = shell_command(&verify);
    c.current_dir(cwd);
    let secs = int_or(step, "verify_timeout", 900).max(1) as u64;
    let r = run_capture(&mut c, None, Duration::from_secs(secs))?;
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );
    Ok((r.code == 0, tail(&all, 12)))
}

fn only_report_missing(missing: &[String]) -> bool {
    missing
        .iter()
        .all(|m| REPORT_ONLY_MISSING.contains(&m.as_str()))
}

/// Python'daki `resolve(root, rel)` (plan kökü dışını reddeden çözücü).
pub type Resolver = fn(&Path, &str) -> Result<PathBuf, String>;

/// Dar anlamsal görev adımı.
pub fn op_agent(
    step: &Step,
    root: &Path,
    dry: bool,
    resolve: Resolver,
    rt: &Runtime,
) -> Result<String, String> {
    if dry {
        return Ok("agent adımı (kuru çalıştırmada atlandı)".into());
    }
    let label = slug(&str_of(step, "_label").unwrap_or_else(|| "agent".into()));
    let repo = resolve(root, &str_of(step, "repo").unwrap_or_else(|| ".".into()))?;
    let repo_is_git = is_git(&repo);
    if (flag(step, "worktree", false) || flag(step, "commit", false) || flag(step, "push", false))
        && !repo_is_git
    {
        return Err("worktree/commit/push için `repo` (git deposu) belirtilmeli; belirtilmezse sözleşme sessizce devre dışı kalıyordu".into());
    }
    let need_changes = flag(step, "require_changes", true) && repo_is_git;
    let need_commit = flag(step, "commit", false) && repo_is_git;
    let report = str_of(step, "report").filter(|s| !s.is_empty());
    let use_wt = flag(step, "worktree", false) && repo_is_git && repo != root;
    let workdir = match str_of(step, "workdir").filter(|s| !s.is_empty()) {
        Some(w) if !use_wt => Some(resolve(root, &w)?),
        _ => None,
    };
    let mut attempts = int_or(step, "retries", 1) + 1;

    let mut work_root = workdir.clone().unwrap_or_else(|| root.to_path_buf());
    let mut work_repo = repo.clone();
    let mut branch = String::new();
    let mut base = String::new();
    let mut resumed = false;
    if use_wt {
        base = head(&repo);
        branch = format!("evren/{label}");
        let wt = root.join(".evren").join("worktrees").join(&label);
        if wt.exists() && is_git(&wt) && git(&repo, &["rev-parse", "--verify", &branch]).code == 0 {
            // Öldürülüp aynı etiketle yeniden başlatılan plan: önceki işin korunur, ajan devam eder.
            resumed = true;
            let mb = git(&repo, &["merge-base", &branch, &base]).stdout();
            if !mb.is_empty() {
                base = mb;
            }
        } else {
            let wt_s = wt.display().to_string();
            if wt.exists() {
                git(&repo, &["worktree", "remove", "--force", &wt_s]);
                let _ = std::fs::remove_dir_all(&wt);
            }
            git(&repo, &["worktree", "prune"]);
            git(&repo, &["branch", "-D", &branch]);
            if let Some(parent) = wt.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let r = git_t(
                &repo,
                &["worktree", "add", "-b", &branch, &wt_s, &base],
                300,
            );
            if r.code != 0 {
                return Err(format!(
                    "git worktree add -b {branch} {wt_s} {base}: {}",
                    last_chars(&r.combined(), 300)
                ));
            }
        }
        exclude_artifacts(&repo);
        work_root = wt.clone();
        work_repo = wt;
    }

    let mut prompt = str_of(step, "prompt").ok_or("'prompt'")?;
    if use_wt {
        let rp = repo.display().to_string();
        let rc: Vec<char> = rp.chars().collect();
        let msys = if rc.len() > 2 && rc[1] == ':' {
            format!(
                "/{}{}",
                rc[0].to_lowercase(),
                rc[2..].iter().collect::<String>().replace('\\', "/")
            )
        } else {
            rp.clone()
        };
        let mut variants: Vec<String> = BTreeSet::from([
            rp.clone(),
            rp.replace('\\', "/"),
            rp.replace('/', "\\"),
            msys,
        ])
        .into_iter()
        .collect();
        variants.sort_by_key(|v| std::cmp::Reverse(v.chars().count()));
        let target = fwd(&work_repo);
        for v in variants {
            if let Ok(re) = Regex::new(&format!("(?i){}", regex::escape(&v))) {
                prompt = re
                    .replace_all(&prompt, regex::NoExpand(&target))
                    .into_owned();
            }
        }
        let repo_f = rp.replace('\\', "/");
        let short = format!(
            "/{}{}",
            rc.first()
                .map(|c| c.to_lowercase().to_string())
                .unwrap_or_default(),
            rc.iter().skip(2).collect::<String>().replace('\\', "/")
        );
        prompt.push_str(&format!(
            "\n\n[ÇALIŞMA DİZİNİ] Bu görev geçici bir kopyada koşuyor: repo kökü {target}. \
             Yalnız bu dizinde çalış; üst dizinlere/diğer repolara dokunma. ANA AĞAÇ YASAK: \
             {repo_f} (ve {short}) dizinine ASLA yazma, \
             orada derleme/komut çalıştırma; tüm komutları (cd dahil) yukarıdaki repo kökünde çalıştır."
        ));
    }
    let want_outcome = flag(step, "outcome", need_changes || need_commit);
    let report_in_agent: Option<String> = report.as_ref().map(|r| {
        if use_wt || workdir.is_some() {
            ".evren-rapor.md".to_owned()
        } else {
            r.clone()
        }
    });
    if let Some(ria) = &report_in_agent {
        prompt.push_str(&format!(
            "\n\n[RAPOR] Sonucu (kök neden, yapılan değişiklik, doğrulama, kalan iş, varsayımlar) {} dosyasına yaz{}{}",
            fwd(&work_root.join(ria)),
            if use_wt { " ve bu dosyayı commit'e KATMA." } else { "." },
            if want_outcome { OUTCOME_DIRECTIVE } else { "" }
        ));
    }
    // Kod değiştiren ve rapor yazan ajanlarda uçtan uca iz zorunlu (`trace: false` ile kapatılır).
    let want_trace = flag(
        step,
        "trace",
        report.is_some() && (need_changes || need_commit),
    );
    if want_trace {
        prompt.push_str(TRACE_DIRECTIVE);
    }
    if let Some(wd) = &workdir {
        prompt.push_str(&format!(
            "\n\n[ÇALIŞMA DİZİNİ] Çalışma alanın {} ile sınırlı (aramalar hızlı olsun diye); üst dizinlere erişme.",
            fwd(wd)
        ));
    }

    let report_path = report.as_ref().map(|r| root.join(r));
    if let Some(rp) = &report_path {
        if rp.is_file() {
            let _ = std::fs::remove_file(rp); // eski sonuç yeni koşunun sonucu sayılmasın
        }
    }
    let mut before = if need_changes || need_commit {
        state(&work_repo)
    } else {
        None
    };
    let mut head_before = if repo_is_git {
        head(&work_repo)
    } else {
        String::new()
    };
    if resumed {
        // Sözleşme ölçütü: çalışma kopyasının taban commit'ine göre fark.
        before = Some(format!("{base}\n"));
        head_before = base.clone();
        prompt.push_str(
            "\n\n[DEVAM] Bu görev daha önce başlatılmış ve yarıda kesilmişti: çalışma kopyasında önceki işin değişiklikleri/commit'leri \
             duruyor. `git status` ve `git log` ile ne yapıldığını gör, baştan başlama, kaldığın yerden tamamla.",
        );
    }
    let head_before_main = if repo_is_git {
        head(&repo)
    } else {
        String::new()
    };
    let changed = |work_repo: &Path, before: &Option<String>, head_before: &str| {
        head(work_repo) != head_before || state(work_repo) != *before
    };
    let mut notes: Vec<String> = Vec::new();
    let mut last = "tamam".to_owned();
    let mut problems = String::new();
    let mut cur_max = int_or(step, "max_steps", 80);
    let mut attempt: i64 = -1;
    let mut report_bonus = 1; // kod işi tamam, yalnız rapor eksikse bir kısa ek tur
    let model = str_of(step, "model").filter(|s| !s.is_empty());
    let commit_msg = |suffix: &str| {
        str_of(step, "commit_message")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("agent: {label}{suffix}"))
    };
    let finish_wt = |merged: &mut String| -> Result<(), String> {
        if use_wt {
            if head(&work_repo) != base {
                *merged = format!(
                    " [ana ağaca alındı: {}]",
                    merge_back(&repo, &branch, &base)?
                );
            }
            git(
                &repo,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    &work_repo.display().to_string(),
                ],
            );
            git(&repo, &["branch", "-D", &branch]);
        }
        Ok(())
    };
    while attempt + 1 < attempts {
        attempt += 1;
        let mut cmd = rt.exe.clone();
        cmd.extend([
            "-w".to_owned(),
            work_root.display().to_string(),
            "--yes".to_owned(),
            "--max-steps".to_owned(),
            cur_max.to_string(),
        ]);
        if let Some(m) = &model {
            cmd.extend(["-m".to_owned(), m.clone()]);
        }
        cmd.extend([
            "-p".to_owned(),
            format!("{prompt}{HEADLESS_DIRECTIVE}{}", notes.concat()),
        ]);
        let logpath = root
            .join("reports")
            .join("agent-logs")
            .join(format!("{label}-a{}.log", attempt + 1));
        let env = agent_env(step);
        let timeout = int_or(step, "timeout", 1800).max(0) as u64;
        let (rc, text) = rt
            .runner
            .run_once(&cmd, &logpath, &label, timeout, env.as_ref())?;
        if rc != 0 && text.to_lowercase().contains("maksimum adım") && attempt + 1 < attempts {
            // Adım sınırı hata değil, devam sinyalidir: sınırı %50 artırıp kaldığı yerden sürdür.
            cur_max = (cur_max as f64 * 1.5) as i64;
            notes = vec![NOTE_STEP_LIMIT.to_owned()];
            problems = "adım sınırı".into();
            say(&format!(
                "  ! {label} deneme {}/{attempts} adım sınırına takıldı; sınır {cur_max}'e çıkarılıp devam ediliyor",
                attempt + 1
            ));
            if use_wt && dirty(&work_repo) {
                git(&work_repo, &["add", "-A"]);
                git(
                    &work_repo,
                    &["commit", "-q", "-m", &commit_msg(" (ara kayıt)")],
                );
            }
            continue;
        }
        if rc != 0 {
            return Err(format!(
                "{}\n(günlük: {})",
                last_chars(text.trim(), 400),
                logpath.display()
            ));
        }
        last = first_chars(text.trim().lines().last().unwrap_or("tamam"), 200).to_owned();
        let rel_log = format!("reports/agent-logs/{label}-a{}.log", attempt + 1);

        let mut missing: Vec<String> = Vec::new();
        if let (Some(wd), Some(ria), Some(rp)) = (&workdir, &report_in_agent, &report_path) {
            if wd.join(ria).exists() {
                move_file(&wd.join(ria), rp)?;
            }
        }
        if use_wt {
            // Rapor ajan kopyasından ana kökteki yerine taşınır; kalan değişiklikleri Evren commit eder.
            if let (Some(ria), Some(rp)) = (&report_in_agent, &report_path) {
                if work_repo.join(ria).exists() {
                    move_file(&work_repo.join(ria), rp)?;
                }
            }
            if dirty(&work_repo) {
                git(&work_repo, &["add", "-A"]);
                git(&work_repo, &["commit", "-q", "-m", &commit_msg("")]);
            }
        }
        if let (Some(r), Some(rp)) = (&report, &report_path) {
            if !need_changes
                && !need_commit
                && flag(step, "report_fallback", true)
                && !nonempty_file(rp)
                && !text.trim().is_empty()
            {
                // Salt rapor adımı: ajan raporu yazamadıysa bulgular kaybolmasın.
                write_text(
                    rp,
                    &format!(
                        "# Ajan raporu yazmadı; Evren son çıktısını kaydetti\n\n{}\n",
                        last_chars(text.trim(), 12000)
                    ),
                )?;
                say(&format!(
                    "  ! {label}: rapor yazılmadı, ajanın son çıktısı rapora kaydedildi ({r})"
                ));
            }
        }
        let mut outcome = report_path.as_deref().and_then(report_outcome);
        if outcome == Some("unk") && report_path.as_deref().is_some_and(is_skeleton_report) {
            outcome = None; // iskelet rapor geçerli "BELİRSİZ" sonucu sayılmaz
        }
        let no_change_ok = outcome.is_some_and(|o| NO_CHANGE_OUTCOMES.contains(&o));
        if need_changes
            && !no_change_ok
            && state(&work_repo) == before
            && head(&work_repo) == head_before
        {
            missing.push("değişiklik yok".into());
            notes = vec![format!("{NOTE_NO_CHANGE}{}", previous_findings(&text))];
        } else if need_commit && !no_change_ok && head(&work_repo) == head_before {
            missing.push("commit yok".into());
            notes = vec![NOTE_NO_COMMIT.to_owned()];
        }
        if let (Some(r), Some(rp)) = (&report, &report_path) {
            let name = Path::new(r)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !nonempty_file(rp) {
                missing.push("rapor yok".into());
                notes.push(NOTE_NO_REPORT.into());
            } else if outcome.is_none() && want_outcome {
                missing.push("rapor sonuç satırı yok".into());
                notes.push(NOTE_NO_OUTCOME.into());
            } else if (flag(step, "require_evidence", false) || name.contains("dogrula"))
                && !report_has_evidence(rp)
            {
                missing.push("KANIT bölümü yok".into());
                notes.push(NOTE_NO_EVIDENCE.into());
            } else if want_trace
                && matches!(outcome, Some("fixed" | "partial"))
                && !report_has_trace(rp)
            {
                missing.push("VERİ YOLU bölümü yok".into());
                notes.push(NOTE_NO_TRACE.into());
            }
        }
        if missing.is_empty()
            && flag(step, "verify", false)
            && !no_change_ok
            && (!repo_is_git || changed(&work_repo, &before, &head_before))
        {
            // Doğrulama kapısı: değişiklik yapan ajanın işi ana ağaca/push'a girmeden doğrulanır.
            let (ok, vtail) = run_verify(step, &work_repo)?;
            if !ok {
                missing.push("doğrulama (verify) başarısız".into());
                notes = vec![format!(
                    "\n\n[UYARI] Doğrulama komutu başarısız oldu (`{}`). Hatayı düzelt, \
                     aynı komutla derlendiğini doğrula, sonra commit et. Çıktı:\n{vtail}",
                    first_chars(&str_of(step, "verify").unwrap_or_default(), 120)
                )];
            }
        }
        if !missing.is_empty() && asks_question(&text) {
            missing.push("soru sorarak bitti".into());
            notes.push(NOTE_QUESTION.into());
        }
        let report_tag = report
            .as_ref()
            .map(|r| format!(" [rapor: {r}]"))
            .unwrap_or_default();
        if missing.is_empty() {
            let suffix = format!(
                " [günlük: {rel_log}]{report_tag}{}",
                outcome
                    .map(|o| format!(" [sonuç: {o}]"))
                    .unwrap_or_default()
            );
            let mut merged = String::new();
            finish_wt(&mut merged)?;
            let mut pushed = String::new();
            if flag(step, "push", false) && repo_is_git && head(&repo) != head_before_main {
                if let Some(hook) = &rt.push_hook {
                    pushed = format!(" [push: {}]", hook(step)?);
                }
            }
            let prefix = if attempt > 0 {
                format!("(deneme {}) ", attempt + 1)
            } else {
                String::new()
            };
            return Ok(format!("{prefix}{last}{merged}{pushed}{suffix}"));
        }
        if only_report_missing(&missing)
            && report_bonus > 0
            && repo_is_git
            && changed(&work_repo, &before, &head_before)
        {
            // Kod commit edildi, eksik yalnız rapor: kısa bir rapor turu.
            report_bonus -= 1;
            attempts += 1;
            cur_max = cur_max.min(20);
            let mut n = vec![NOTE_REPORT_ONLY.to_owned()];
            n.append(&mut notes);
            notes = n;
        }
        if attempt + 1 >= attempts
            && only_report_missing(&missing)
            && repo_is_git
            && changed(&work_repo, &before, &head_before)
            && !text.trim().is_empty()
        {
            // Son hak da yalnız rapor yüzünden düştüyse derlenen kod çöpe gitmesin.
            let ok = if flag(step, "verify", false) {
                run_verify(step, &work_repo)?.0
            } else {
                true
            };
            if ok {
                let range = format!("{head_before}..HEAD");
                let stat = git(&work_repo, &["log", "--stat", "--format=%h %s", &range]).stdout();
                if let Some(rp) = &report_path {
                    write_text(
                        rp,
                        &format!(
                            "SONUÇ: KISMİ (ajan raporu yazmadı; Evren son çıktı ve git özetinden üretti)\n\n\
                             ## Yapılan commit'ler\n```\n{}\n```\n\n## Ajanın son çıktısı\n{}\n",
                            last_chars(&stat, 4000),
                            last_chars(text.trim(), 8000)
                        ),
                    )?;
                }
                say(&format!(
                    "  ! {label}: kod doğrulandı, rapor ajan çıktısından üretildi ({})",
                    missing.join(", ")
                ));
                let mut merged = String::new();
                finish_wt(&mut merged)?;
                return Ok(format!(
                    "(deneme {}) {last}{merged} [günlük: {rel_log}]{report_tag} [sonuç: partial]",
                    attempt + 1
                ));
            }
        }
        problems = missing.join(", ");
        say(&format!(
            "  ! {label} deneme {}/{attempts} eksik: {problems}",
            attempt + 1
        ));
    }
    Err(format!(
        "ajan sözleşmeyi karşılamadı ({problems}); son çıktı: {last} (günlük: reports/agent-logs/{label}-a*.log){}",
        if use_wt {
            format!("; yarım iş dalı: {branch}")
        } else {
            String::new()
        }
    ))
}

/// `EVREN_MAX_PARALLEL` (varsayılan 6).
pub fn parallel_cap() -> usize {
    match std::env::var("EVREN_MAX_PARALLEL") {
        Ok(v) => v
            .trim()
            .parse::<i64>()
            .map(|n| n.max(1) as usize)
            .unwrap_or(6),
        Err(_) => 6,
    }
}

/// Tutulan süreçler arası ajan slotu; düşünce bırakılır.
pub struct Slot {
    #[allow(dead_code)]
    pub index: usize,
    _file: File,
}

/// Süreçler arası ajan slotu: aynı makinedeki tüm evren süreçleri toplam
/// `cap` ajan koşturur (`slot-N.lock` dosya kilidi).
pub fn agent_slot(dir: &Path, cap: usize) -> Slot {
    let _ = std::fs::create_dir_all(dir);
    loop {
        for i in 0..cap {
            let Ok(fh) = OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(dir.join(format!("slot-{i}.lock")))
            else {
                continue;
            };
            if fh.try_lock().is_ok() {
                return Slot {
                    index: i,
                    _file: fh,
                };
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Alt adım çalıştırıcı: adım adı ve adımı alır; bilinmeyen ad için `None`.
pub type OpFn<'a> = dyn Fn(&str, &Step) -> Option<Result<String, String>> + Sync + 'a;

/// Alt adımları eşzamanlı koşturur (`max_workers`, varsayılan 3; tavan
/// EVREN_MAX_PARALLEL=6). Hepsi bitince hataları toplayıp bildirir.
pub fn op_parallel(
    step: &Step,
    dry: bool,
    ops: &OpFn<'_>,
    ctx: &HashMap<String, String>,
    slots_dir: &Path,
) -> Result<String, String> {
    let subs: Vec<Value> = match step.get("steps") {
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err("'steps' bir liste olmalı".into()),
        None => return Err("'steps'".into()),
    };
    let cap = parallel_cap();
    let workers = (int_or(step, "max_workers", 3).max(1) as usize).min(cap);
    let base_label = str_of(step, "_label").unwrap_or_else(|| "parallel".into());
    let n = subs.len();

    let run = |j: usize, sub: &Value| -> Result<String, String> {
        let mut sub = expand(sub, ctx).as_object().cloned().unwrap_or_default();
        sub.insert("_label".into(), Value::String(format!("{base_label}.{j}")));
        let op = sub.get("op").map(py_str).unwrap_or_else(|| "None".into());
        if op == "agent" {
            sub.entry("worktree").or_insert(Value::Bool(true));
        }
        let _slot = (op == "agent" && !dry).then(|| agent_slot(slots_dir, cap));
        ops(&op, &sub).unwrap_or_else(|| Err(format!("parallel içinde desteklenmeyen adım: {op}")))
    };

    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Result<String, String>)>();
    let mut results: Vec<Option<Result<String, String>>> = (0..n).map(|_| None).collect();
    let mut errors: Vec<String> = Vec::new();
    std::thread::scope(|s| {
        for _ in 0..workers.min(n.max(1)) {
            let tx = tx.clone();
            let (next, subs, run) = (&next, &subs, &run);
            s.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(sub) = subs.get(i) else { break };
                let _ = tx.send((i, run(i + 1, sub)));
            });
        }
        drop(tx);
        let mut shown = 0;
        for (i, res) in rx {
            if let Some(slot) = results.get_mut(i) {
                *slot = Some(res);
            }
            while let Some(Some(res)) = results.get(shown) {
                let j = shown + 1;
                match res {
                    Ok(out) => say(&format!("  ✓ alt adım {base_label}.{j}: {out}")),
                    Err(e) => {
                        errors.push(format!("{j}: {e}"));
                        say(&format!("  ✗ alt adım {base_label}.{j}: {e}"));
                    }
                }
                shown += 1;
            }
        }
    });
    if !errors.is_empty() {
        return Err(format!(
            "{}/{n} alt adım başarısız:\n{}",
            errors.len(),
            errors.join("\n")
        ));
    }
    Ok(format!("{n} alt adım tamam"))
}

#[cfg(test)]
mod tests;
