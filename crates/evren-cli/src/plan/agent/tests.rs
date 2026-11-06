//! Python `tests/test_plan_agent.py` karşılıkları (sahte ajan çalıştırıcısıyla).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use evren_core::tools::file_ops::resolve_path;
use serde_json::json;
use std::sync::{Arc, Mutex};

type Act = dyn Fn(&[String], Option<&BTreeMap<String, String>>) -> Result<(i32, String), String>
    + Send
    + Sync;

struct Fake(Box<Act>);

impl AgentRunner for Fake {
    fn run_once(
        &self,
        cmd: &[String],
        log: &Path,
        _label: &str,
        _timeout: u64,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<(i32, String), String> {
        let res = (self.0)(cmd, env);
        if let Ok((_, text)) = &res {
            std::fs::create_dir_all(log.parent().unwrap()).unwrap();
            std::fs::write(log, text).unwrap();
        }
        res
    }
}

fn arg(cmd: &[String], name: &str) -> String {
    let i = cmd.iter().position(|c| c == name).unwrap();
    cmd[i + 1].clone()
}

fn wd(cmd: &[String]) -> PathBuf {
    PathBuf::from(arg(cmd, "-w"))
}

fn g(repo: &Path, args: &[&str]) {
    let st = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

fn g_out(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    Ok(resolve_path(&root.join(rel)))
}

struct Ws {
    _tmp: tempfile::TempDir,
    _slots: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    slots: PathBuf,
}

fn ws() -> Ws {
    let tmp = tempfile::tempdir().unwrap();
    let slots = tempfile::tempdir().unwrap();
    let root = resolve_path(tmp.path());
    let repo = root.join("app");
    std::fs::create_dir(&repo).unwrap();
    g(&repo, &["init", "-q", "-b", "main"]);
    g(&repo, &["config", "user.email", "t@t"]);
    g(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "1").unwrap();
    g(&repo, &["add", "-A"]);
    g(&repo, &["commit", "-q", "-m", "init"]);
    let slots_dir = slots.path().to_path_buf();
    Ws {
        _tmp: tmp,
        _slots: slots,
        root,
        repo,
        slots: slots_dir,
    }
}

fn rt(w: &Ws, act: Box<Act>, push: Option<PushHook>) -> Runtime {
    Runtime {
        exe: vec!["evren".into()],
        runner: Box::new(Fake(act)),
        push_hook: push,
        slots_dir: w.slots.clone(),
        skills_dir: w.slots.join("skills"),
    }
}

/// Python `_fake`: eylemi çalışma dizininde yapar, `text` döndürür.
fn fake(w: &Ws, action: impl Fn(&Path) + Send + Sync + 'static, text: &str) -> Runtime {
    let text = text.to_owned();
    rt(
        w,
        Box::new(move |cmd, _| {
            action(&wd(cmd));
            Ok((0, text.clone()))
        }),
        None,
    )
}

fn step(v: Value) -> Step {
    v.as_object().cloned().unwrap()
}

fn run(w: &Ws, s: Value, r: &Runtime) -> Result<String, String> {
    op_agent(&step(s), &w.root, false, resolve, r)
}

#[test]
fn asks_question_cases() {
    assert!(asks_question("Rapor hazır.\nHangisiyle devam edeyim?"));
    assert!(asks_question("kalan 3 iş için onayın var mı?"));
    assert!(!asks_question(
        "Neden? Çünkü X.\nTamamlandı: commit abc123."
    ));
    assert!(!asks_question(""));
}

#[test]
fn worktree_merges_and_cleans() {
    let w = ws();
    let r = fake(
        &w,
        |wd| {
            std::fs::write(wd.join("b.txt"), "2").unwrap();
            g(wd, &["add", "b.txt"]);
            g(wd, &["commit", "-q", "-m", "yeni"]);
        },
        "bitti",
    );
    let prompt = format!("{} içinde çalış", w.repo.display());
    let out = run(
        &w,
        json!({"prompt": prompt, "repo": "app", "worktree": true, "commit": true, "_label": "t-1"}),
        &r,
    )
    .unwrap();
    assert!(
        out.contains("ana ağaca alındı") && out.contains("reports/agent-logs/t-1-a1.log"),
        "{out}"
    );
    assert_eq!(std::fs::read_to_string(w.repo.join("b.txt")).unwrap(), "2");
    assert!(!w.root.join(".evren").join("worktrees").join("t-1").exists());
}

#[test]
fn auto_commit_message() {
    let w = ws();
    let r = fake(
        &w,
        |wd| std::fs::write(wd.join("c.txt"), "3").unwrap(),
        "bitti",
    );
    run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true,
               "commit_message": "evren commit", "_label": "t-2"}),
        &r,
    )
    .unwrap();
    assert_eq!(
        g_out(&w.repo, &["log", "-1", "--format=%s"]),
        "evren commit"
    );
}

#[test]
fn question_without_change_fails_after_retry() {
    let w = ws();
    let r = fake(&w, |_| {}, "Devam edeyim mi?");
    let err = run(
        &w,
        json!({"prompt": "x", "repo": "app", "_label": "t-3"}),
        &r,
    )
    .unwrap_err();
    assert!(err.contains("sözleşmeyi karşılamadı"), "{err}");
    assert!(w.root.join("reports/agent-logs/t-3-a2.log").exists());
}

#[test]
fn report_without_git() {
    let w = ws();
    let r = fake(
        &w,
        |wd| {
            std::fs::create_dir_all(wd.join("reports")).unwrap();
            std::fs::write(wd.join("reports/r.md"), "sonuç").unwrap();
        },
        "bitti",
    );
    let out = run(
        &w,
        json!({"prompt": "x", "repo": ".", "require_changes": false, "report": "reports/r.md", "_label": "t-4"}),
        &r,
    )
    .unwrap();
    assert!(out.contains("rapor: reports/r.md"), "{out}");
}

fn push_recorder() -> (Arc<Mutex<Vec<String>>>, PushHook) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c2 = Arc::clone(&calls);
    let hook: PushHook = Box::new(move |s: &Step| {
        c2.lock()
            .unwrap()
            .push(str_of(s, "repo").unwrap_or_default());
        Ok("ok".into())
    });
    (calls, hook)
}

#[test]
fn push_after_merge() {
    let w = ws();
    let (calls, hook) = push_recorder();
    let r = rt(
        &w,
        Box::new(|cmd, _| {
            std::fs::write(wd(cmd).join("d.txt"), "4").unwrap();
            Ok((0, "bitti".into()))
        }),
        Some(hook),
    );
    let out = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true, "push": true, "_label": "t-5"}),
        &r,
    )
    .unwrap();
    assert_eq!(*calls.lock().unwrap(), vec!["app".to_owned()]);
    assert!(out.contains("[push: ok]"), "{out}");
}

#[test]
fn push_skipped_without_change() {
    let w = ws();
    let (calls, hook) = push_recorder();
    let r = rt(&w, Box::new(|_, _| Ok((0, "bitti".into()))), Some(hook));
    assert!(run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "push": true, "_label": "t-6"}),
        &r
    )
    .is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn contract_needs_repo() {
    let w = ws();
    let r = fake(&w, |_| {}, "bitti");
    let err = run(
        &w,
        json!({"prompt": "x", "worktree": true, "commit": true, "_label": "t-7"}),
        &r,
    )
    .unwrap_err();
    assert!(err.contains("repo"), "{err}");
}

#[test]
fn dirty_main_is_stashed() {
    let w = ws();
    let repo = w.repo.clone();
    let r = fake(
        &w,
        move |wd| {
            std::fs::write(repo.join("kirli.txt"), "x").unwrap();
            std::fs::write(wd.join("e.txt"), "5").unwrap();
        },
        "bitti",
    );
    let out = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true, "_label": "t-8"}),
        &r,
    )
    .unwrap();
    assert!(out.contains("stash'e alındı"), "{out}");
    assert!(!w.repo.join("kirli.txt").exists());
    assert!(w.repo.join("e.txt").exists());
    assert!(g_out(&w.repo, &["stash", "list"]).contains("evren-ana-agac-kirlendi"));
}

#[test]
fn msys_path_rewritten_to_worktree() {
    let w = ws();
    let seen = Arc::new(Mutex::new(String::new()));
    let s2 = Arc::clone(&seen);
    let r = rt(
        &w,
        Box::new(move |cmd, _| {
            *s2.lock().unwrap() = arg(cmd, "-p");
            std::fs::write(wd(cmd).join("f.txt"), "6").unwrap();
            Ok((0, "bitti".into()))
        }),
        None,
    );
    let repo = w.repo.display().to_string();
    let msys = format!(
        "/{}{}",
        repo[..1].to_lowercase(),
        repo[2..].replace('\\', "/")
    );
    run(
        &w,
        json!({"prompt": format!("cd {msys} && build"), "repo": "app", "worktree": true,
               "commit": true, "_label": "t-9"}),
        &r,
    )
    .unwrap();
    let prompt = seen.lock().unwrap().replace('\\', "/");
    let head = prompt.split("ANA AĞAÇ").next().unwrap();
    assert!(head.contains(".evren/worktrees/t-9"), "{head}");
}

#[test]
fn workdir_without_worktree() {
    let w = ws();
    let seen = Arc::new(Mutex::new((PathBuf::new(), String::new())));
    let s2 = Arc::clone(&seen);
    let r = rt(
        &w,
        Box::new(move |cmd, _| {
            let d = wd(cmd);
            std::fs::write(d.join(".evren-rapor.md"), "teshis").unwrap();
            *s2.lock().unwrap() = (d, arg(cmd, "-p"));
            Ok((0, "bitti".into()))
        }),
        None,
    );
    let out = run(
        &w,
        json!({"prompt": "x", "repo": ".", "workdir": "app", "worktree": false,
               "require_changes": false, "report": "reports/t.md", "_label": "t-10"}),
        &r,
    )
    .unwrap();
    let (d, prompt) = seen.lock().unwrap().clone();
    assert_eq!(d, w.repo);
    assert_eq!(
        std::fs::read_to_string(w.root.join("reports/t.md")).unwrap(),
        "teshis"
    );
    assert!(out.contains("rapor: reports/t.md"), "{out}");
    assert!(prompt.contains("HIZ KURALLARI"));
}

#[test]
fn killed_attempt_resumes_in_same_worktree() {
    let w = ws();
    let n = Arc::new(Mutex::new(0));
    let checks = Arc::new(Mutex::new(Vec::new()));
    let (n2, c2) = (Arc::clone(&n), Arc::clone(&checks));
    let r = rt(
        &w,
        Box::new(move |cmd, _| {
            let d = wd(cmd);
            let mut k = n2.lock().unwrap();
            *k += 1;
            if *k == 1 {
                std::fs::write(d.join("yarim.txt"), "onceki is").unwrap();
                return Err("öldürüldü".into());
            }
            c2.lock()
                .unwrap()
                .push(d.join("yarim.txt").exists() && arg(cmd, "-p").contains("[DEVAM]"));
            std::fs::write(d.join("tamam.txt"), "son").unwrap();
            Ok((0, "ok".into()))
        }),
        None,
    );
    let s =
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true, "_label": "t-11"});
    assert!(run(&w, s.clone(), &r).is_err());
    let out = run(&w, s, &r).unwrap();
    assert_eq!(*checks.lock().unwrap(), vec![true]);
    assert!(out.contains("ana ağaca alındı"), "{out}");
    assert!(w.repo.join("yarim.txt").exists() && w.repo.join("tamam.txt").exists());
}

#[test]
fn find_bash_prefers_git_bash() {
    let found = super::super::find_bash();
    if cfg!(windows) {
        assert!(found.is_none_or(|p| !p.display().to_string().to_lowercase().contains("system32")));
    } else {
        assert!(found.is_some());
    }
}

/// Python `_report_fake`.
fn report_fake(w: &Ws, line: &str, change: bool) -> Runtime {
    let line = line.to_owned();
    rt(
        w,
        Box::new(move |cmd, _| {
            let d = wd(cmd);
            if change {
                std::fs::write(d.join("z.txt"), "1").unwrap();
            }
            if arg(cmd, "-p").contains("[RAPOR]") {
                std::fs::write(d.join(".evren-rapor.md"), format!("{line}\n\nGerekçe...")).unwrap();
            }
            Ok((0, "ok".into()))
        }),
        None,
    )
}

#[test]
fn outcome_parser() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("r.md");
    for (line, key) in [
        ("SONUÇ: ZATEN ÇÖZÜLMÜŞ (commit abc)", "done"),
        ("**SONUÇ:** FRONTEND", "front"),
        ("Sonuç: Kütüphane değişikliği gerekli", "lib"),
        ("# SONUÇ: Belirsiz", "unk"),
        ("sonuç: düzeltildi", "fixed"),
    ] {
        std::fs::write(&f, format!("{line}\nx")).unwrap();
        assert_eq!(report_outcome(&f), Some(key), "{line}");
    }
}

#[test]
fn no_change_outcome_passes_contract() {
    let w = ws();
    let r = report_fake(&w, "SONUÇ: FRONTEND", false);
    let out = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true,
               "report": "reports/r.md", "_label": "t-12"}),
        &r,
    )
    .unwrap();
    assert!(
        out.contains("[sonuç: front]") && w.root.join("reports/r.md").exists(),
        "{out}"
    );
    assert!(!out.contains("ana ağaca alındı"));
}

#[test]
fn fixed_outcome_without_change_fails() {
    let w = ws();
    let r = report_fake(&w, "SONUÇ: DÜZELTİLDİ", false);
    let err = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true,
               "report": "reports/r2.md", "_label": "t-13"}),
        &r,
    )
    .unwrap_err();
    assert!(
        err.contains("değişiklik yok") || err.contains("commit yok"),
        "{err}"
    );
}

/// Önceden kirli dosyayı tekrar düzenleyen ajan "değişiklik yok" almamalı ve yeniden denenmemeli.
fn edit_dirty_file_once(w: &Ws, file: &'static str) {
    let calls = Arc::new(Mutex::new(0usize));
    let c = calls.clone();
    let repo = w.repo.clone();
    let r = fake(
        w,
        move |_| {
            *c.lock().unwrap() += 1;
            let p = repo.join(file);
            let mut s = std::fs::read_to_string(&p).unwrap_or_default();
            s.push_str("\nek satır");
            std::fs::write(&p, s).unwrap();
        },
        "Tamamlandı.",
    );
    let res = run(w, json!({"prompt": "x", "repo": "app", "retries": 2}), &r);
    if let Err(e) = &res {
        assert!(!e.contains("değişiklik yok"), "{e}");
    }
    assert_eq!(*calls.lock().unwrap(), 1, "{res:?}");
}

#[test]
fn edit_of_preexisting_untracked_file_counts_as_change() {
    let w = ws();
    std::fs::write(w.repo.join("notes.md"), "not").unwrap();
    edit_dirty_file_once(&w, "notes.md");
}

#[test]
fn edit_of_preexisting_modified_file_counts_as_change() {
    let w = ws();
    std::fs::write(w.repo.join("a.txt"), "2").unwrap();
    edit_dirty_file_once(&w, "a.txt");
}

#[test]
fn agent_doing_nothing_is_no_change() {
    let w = ws();
    std::fs::write(w.repo.join("notes.md"), "not").unwrap();
    let r = fake(&w, |_| {}, "Tamamlandı.");
    let err = run(&w, json!({"prompt": "x", "repo": "app", "retries": 0}), &r).unwrap_err();
    assert!(err.contains("değişiklik yok"), "{err}");
}

#[test]
fn missing_outcome_line_fails() {
    let w = ws();
    let r = report_fake(&w, "Başlık yok", false);
    let err = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true,
               "report": "reports/r3.md", "_label": "t-14"}),
        &r,
    )
    .unwrap_err();
    assert!(err.contains("sonuç satırı"), "{err}");
}

#[test]
fn missing_outcome_with_change_is_partial() {
    let w = ws();
    let r = report_fake(&w, "Başlık yok", true);
    let out = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true,
               "report": "reports/r3b.md", "_label": "t-14b"}),
        &r,
    )
    .unwrap();
    assert!(out.contains("[sonuç: partial]"), "{out}");
    assert!(w.repo.join("z.txt").exists());
    let rep = std::fs::read_to_string(w.root.join("reports/r3b.md")).unwrap();
    assert!(rep.starts_with("SONUÇ: KISMİ"), "{rep}");
}

#[test]
fn step_limit_raises_and_resumes() {
    let w = ws();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let warned = Arc::new(Mutex::new(false));
    let (s2, w2) = (Arc::clone(&seen), Arc::clone(&warned));
    let r = rt(
        &w,
        Box::new(move |cmd, _| {
            let d = wd(cmd);
            let mut v = s2.lock().unwrap();
            v.push(arg(cmd, "--max-steps").parse::<i64>().unwrap());
            if v.len() == 1 {
                std::fs::write(d.join("yarim.txt"), "1").unwrap();
                return Ok((1, "HATA: Ajan maksimum adım sınırına (60) ulaştı".into()));
            }
            *w2.lock().unwrap() = arg(cmd, "-p").contains("[UYARI] Önceki deneme adım sınırına");
            std::fs::write(
                d.join(".evren-rapor.md"),
                "SONUÇ: DÜZELTİLDİ\n\n## VERİ YOLU\n- a.cs:1 uç\n- b.cs:2 servis\n- c.cs:3 depo\n",
            )
            .unwrap();
            std::fs::write(d.join("tamam.txt"), "2").unwrap();
            Ok((0, "ok".into()))
        }),
        None,
    );
    let out = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true, "max_steps": 60,
               "report": "reports/r4.md", "_label": "t-15"}),
        &r,
    )
    .unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![60, 90]);
    assert!(*warned.lock().unwrap());
    assert!(out.contains("[sonuç: fixed]"), "{out}");
    assert!(w.repo.join("yarim.txt").exists() && w.repo.join("tamam.txt").exists());
}

#[test]
fn verify_failure_blocks_merge() {
    let w = ws();
    let r = fake(
        &w,
        |wd| std::fs::write(wd.join("bozuk.txt"), "x").unwrap(),
        "bitti",
    );
    let err = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true,
               "verify": "exit 1", "_label": "t-16"}),
        &r,
    )
    .unwrap_err();
    assert!(err.contains("verify"), "{err}");
    assert!(!w.repo.join("bozuk.txt").exists());
}

#[test]
fn verify_success_merges() {
    let w = ws();
    let r = fake(
        &w,
        |wd| std::fs::write(wd.join("iyi.txt"), "x").unwrap(),
        "bitti",
    );
    let out = run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true,
               "verify": "exit 0", "_label": "t-17"}),
        &r,
    )
    .unwrap();
    assert!(out.contains("ana ağaca alındı"), "{out}");
    assert!(w.repo.join("iyi.txt").exists());
}

#[test]
fn retry_carries_previous_findings() {
    let w = ws();
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let p2 = Arc::clone(&prompts);
    let r = rt(
        &w,
        Box::new(move |cmd, _| {
            let mut v = p2.lock().unwrap();
            v.push(arg(cmd, "-p"));
            if v.len() == 1 {
                let text = format!(
                    "{}**Yapılan:** kök neden Validator.cs:60.\n**Tek somut sonraki adım:** Validator.cs:60-75 düzenle.",
                    "arama çıktısı\n".repeat(50)
                );
                return Ok((0, text));
            }
            std::fs::write(wd(cmd).join("b.txt"), "2").unwrap();
            Ok((0, "bitti".into()))
        }),
        None,
    );
    run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true, "_label": "t-f"}),
        &r,
    )
    .unwrap();
    let v = prompts.lock().unwrap();
    assert!(v[1].contains("ÖNCEKİ DENEMENİN BULGULARI"));
    assert!(v[1].contains("Validator.cs:60-75 düzenle"));
    assert!(!v[1].contains("arama çıktısı"));
}

#[test]
fn env_passed_to_agent() {
    let w = ws();
    let seen = Arc::new(Mutex::new(None));
    let s2 = Arc::clone(&seen);
    let r = rt(
        &w,
        Box::new(move |cmd, env| {
            *s2.lock().unwrap() = env.cloned();
            std::fs::write(wd(cmd).join("c.txt"), "3").unwrap();
            Ok((0, "bitti".into()))
        }),
        None,
    );
    run(
        &w,
        json!({"prompt": "x", "repo": "app", "worktree": true, "commit": true,
               "planner": false, "env": {"EVREN_X": 1}, "_label": "t-e"}),
        &r,
    )
    .unwrap();
    let want: BTreeMap<String, String> = [("EVREN_X", "1"), ("EVREN_AUTO_ROUTE", "0")]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    assert_eq!(*seen.lock().unwrap(), Some(want));
}

#[test]
fn previous_findings_tail() {
    let out = previous_findings(&format!("{}SON", "a".repeat(10000)));
    assert!(out.ends_with("SON") && out.chars().count() < 4000);
    assert_eq!(previous_findings(""), "");
}

#[test]
fn report_ready_for_test_cases() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let write = |name: &str, text: &str| {
        let f = p.join(name);
        std::fs::write(&f, text).unwrap();
        f
    };
    let ok = write(
        "dogrula-a.md",
        "SONUÇ: GEÇTİ

## KANIT

GET /api/x -> 200 totalCount 16
",
    );
    let done = write(
        "dogrula-b.md",
        "SONUÇ: ZATEN ÇÖZÜLMÜŞ

## KANIT

log satırı
",
    );
    let empty = write(
        "dogrula-c.md",
        "SONUÇ: GEÇTİ

## KANIT

## Sonra
",
    );
    let failed = write(
        "dogrula-d.md",
        "SONUÇ: KALDI

## KANIT

500 döndü
",
    );
    assert!(report_ready_for_test(&ok));
    assert!(report_ready_for_test(&done));
    assert!(!report_ready_for_test(&empty));
    assert!(!report_ready_for_test(&failed));
    assert_eq!(report_outcome(&failed), Some("failed"));
    assert!(!report_ready_for_test(&p.join("yok.md")));
}

#[test]
fn parallel_cap_and_slots() {
    std::env::set_var("EVREN_MAX_PARALLEL", "2");
    assert_eq!(parallel_cap(), 2);
    std::env::remove_var("EVREN_MAX_PARALLEL");
    let tmp = tempfile::tempdir().unwrap();
    {
        let a = agent_slot(tmp.path(), 2);
        let b = agent_slot(tmp.path(), 2);
        let got: std::collections::BTreeSet<usize> = [a.index, b.index].into_iter().collect();
        assert_eq!(got, [0, 1].into_iter().collect());
    }
    assert_eq!(agent_slot(tmp.path(), 2).index, 0);
}

#[test]
fn report_kind_needs_no_changes() {
    let w = ws();
    let write = |wd: &Path| {
        std::fs::create_dir_all(wd.join("reports")).unwrap();
        std::fs::write(wd.join("reports/k.md"), "rapor").unwrap();
    };
    let r = fake(&w, write, "bitti");
    let base = json!({"prompt": "x", "repo": "app", "report": "reports/k.md", "retries": 0, "_label": "t-k1"});
    let err = run(&w, base.clone(), &r).unwrap_err();
    assert!(
        err.contains("değişiklik yok") && err.contains("\"kind\": \"report\""),
        "{err}"
    );
    let mut s = base;
    s["kind"] = json!("report");
    s["_label"] = json!("t-k2");
    let out = run(&w, s, &r).unwrap();
    assert!(out.contains("rapor: reports/k.md"), "{out}");
}

#[test]
fn inputs_are_appended_verbatim() {
    let w = ws();
    std::fs::create_dir_all(w.root.join("reports")).unwrap();
    std::fs::write(w.root.join("reports/ozet.txt"), "hata_orani=3.2%\n").unwrap();
    let s = step(json!({"inputs": ["reports/ozet.txt"]}));
    let block = inputs_block(&s, &w.root, resolve).unwrap();
    assert!(
        block.contains("[GİRDİ: reports/ozet.txt]\nhata_orani=3.2%\n[/GİRDİ]"),
        "{block}"
    );
    assert!(block.contains("AYNEN"));
    std::fs::write(w.root.join("reports/bos.txt"), " \n").unwrap();
    let s = step(json!({"inputs": ["reports/bos.txt"]}));
    assert!(inputs_block(&s, &w.root, resolve)
        .unwrap_err()
        .contains("girdi boş"));
}

#[test]
fn state_sees_new_file_beside_nested_repo() {
    // İzlenmeyen iç içe depo (`nested/`) varken var olan izlenmeyen dizine eklenen dosya
    // durumu değiştirmeli; eskiden untracked_hash `None` dönüp değişikliği gizliyordu.
    let w = ws();
    std::fs::create_dir_all(w.repo.join("nested")).unwrap();
    g(&w.repo.join("nested"), &["init", "-q"]);
    std::fs::create_dir_all(w.repo.join("reports")).unwrap();
    std::fs::write(w.repo.join("reports/old.md"), "eski").unwrap();
    assert!(untracked_hash(&w.repo).is_some());
    let r = fake(
        &w,
        |wd| std::fs::write(wd.join("app/reports/new.md"), "yeni").unwrap(),
        "bitti",
    );
    run(
        &w,
        json!({"prompt": "x", "repo": "app", "_label": "t-nested"}),
        &r,
    )
    .unwrap();
}
