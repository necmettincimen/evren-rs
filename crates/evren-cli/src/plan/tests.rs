//! `evren plan` çalıştırıcı adımları (plan_runner.py davranışı).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;

fn step(v: Value) -> Step {
    v.as_object().cloned().unwrap()
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = resolve_path(tmp.path());
    (tmp, root)
}

fn rt(root: &Path) -> Runtime {
    runtime(root, true, false, Box::new(ProcessRunner))
}

#[test]
fn template_translation() {
    assert_eq!(py_template(r"\1-\g<ad>"), "${1}-${ad}");
    assert_eq!(py_template("a$b\\n"), "a$$b\n");
    assert_eq!(py_template(r"\12x"), "${12}x");
}

#[test]
fn glob_sorted_recursive() {
    let (_t, r) = root();
    std::fs::create_dir_all(r.join("a/b")).unwrap();
    std::fs::write(r.join("a/b/z.cs"), "").unwrap();
    std::fs::write(r.join("a/y.cs"), "").unwrap();
    std::fs::write(r.join("a/x.txt"), "").unwrap();
    let got: Vec<String> = glob_sorted(&r, "**/*.cs")
        .iter()
        .map(|p| {
            p.strip_prefix(&r)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(got, vec!["a/b/z.cs", "a/y.cs"]);
}

#[test]
fn replace_regex_literal_and_required() {
    let (_t, r) = root();
    std::fs::write(r.join("f.txt"), "ad = 1\r\nsoyad = 2\r\n").unwrap();
    let out = op_replace(
        &step(json!({"glob": "*.txt", "find": r"(\w+) = 1", "with": r"\1 = 9", "regex": true})),
        &r,
        false,
    )
    .unwrap();
    assert_eq!(out, "1 dosya tarandı, 1 değişti");
    assert_eq!(
        std::fs::read_to_string(r.join("f.txt")).unwrap(),
        "ad = 9\nsoyad = 2\n"
    );
    op_replace(
        &step(json!({"glob": "*.txt", "find": "a$d", "with": "$x"})),
        &r,
        false,
    )
    .unwrap_err();
    std::fs::write(r.join("g.txt"), "a.b\n").unwrap();
    let out = op_replace(
        &step(json!({"glob": "g.txt", "find": "a.b", "with": "\\1$"})),
        &r,
        false,
    )
    .unwrap();
    assert_eq!(out, "1 dosya tarandı, 1 değişti");
    assert_eq!(std::fs::read_to_string(r.join("g.txt")).unwrap(), "\\1$\n");
    let err = op_replace(
        &step(json!({"glob": "*.txt", "find": "yok", "with": "x"})),
        &r,
        false,
    )
    .unwrap_err();
    assert!(
        err.starts_with("replace hicbir dosyada eslesmedi (2 dosya tarandi)"),
        "{err}"
    );
    let ok = op_replace(
        &step(json!({"glob": "*.txt", "find": "yok", "with": "x", "required": false})),
        &r,
        false,
    )
    .unwrap();
    assert_eq!(ok, "2 dosya tarandı, 0 değişti");
}

#[test]
fn write_and_outside_root() {
    let (_t, r) = root();
    let out = op_write(
        &step(json!({"path": "d/e.txt", "content": "merhaba"})),
        &r,
        false,
    )
    .unwrap();
    assert!(out.starts_with("yazıldı: "));
    assert_eq!(
        std::fs::read_to_string(r.join("d/e.txt")).unwrap(),
        "merhaba"
    );
    let err = op_write(
        &step(json!({"path": "../dis.txt", "content": "x"})),
        &r,
        false,
    )
    .unwrap_err();
    assert_eq!(err, "çalışma alanı dışı yol: ../dis.txt");
    assert_eq!(
        op_write(&step(json!({"content": "x"})), &r, false).unwrap_err(),
        "'path'"
    );
}

#[test]
fn run_exit_codes() {
    let (_t, r) = root();
    let out = op_run(&step(json!({"cmd": "echo selam"})), &r, false).unwrap();
    assert_eq!(out, "tamam: selam");
    let err = op_run(&step(json!({"cmd": "exit 3"})), &r, false).unwrap_err();
    assert!(err.starts_with("çıkış kodu 3"), "{err}");
    assert_eq!(
        op_run(&step(json!({"cmd": "exit 3"})), &r, true).unwrap(),
        "çalıştırılacak: exit 3"
    );
}

#[test]
fn commit_rejects_ai_signature() {
    let (_t, r) = root();
    let err = op_commit(
        &step(json!({"repo": ".", "message": "düzelt\n\nCo-Authored-By: x"})),
        &r,
        true,
    )
    .unwrap_err();
    assert_eq!(
        err,
        "commit mesajında AI imzası/attribution yasak (proje kuralı)"
    );
    assert_eq!(
        op_commit(&step(json!({"repo": ".", "message": "düzelt"})), &r, true).unwrap(),
        "commit: düzelt"
    );
}

#[test]
fn push_needs_allow() {
    let (_t, r) = root();
    assert_eq!(
        op_push(&step(json!({"repo": "."})), &r, true, false).unwrap_err(),
        "push adımı için --allow-push gerekir (kullanıcı onayı)"
    );
}

#[test]
fn plan_dry_run_and_errors() {
    let (_t, r) = root();
    let plan = r.join("p.json");
    std::fs::write(
        &plan,
        "\u{feff}{\"steps\": [{\"op\": \"write\", \"path\": \"a.txt\", \"content\": \"x\"},\
         {\"op\": \"run\", \"cmd\": \"echo 1\"}]}",
    )
    .unwrap();
    let p = plan.display().to_string();
    assert_eq!(run_plan_with(&p, &r, true, false, 1, &rt(&r)).unwrap(), 0);
    assert!(!r.join("a.txt").exists());
    assert_eq!(run_plan_with(&p, &r, false, false, 2, &rt(&r)).unwrap(), 0);
    assert!(!r.join("a.txt").exists());

    std::fs::write(&plan, r#"{"steps": [{"op": "uç"}]}"#).unwrap();
    assert_eq!(run_plan_with(&p, &r, false, false, 1, &rt(&r)).unwrap(), 1);
    std::fs::write(&plan, r#"{"adimlar": []}"#).unwrap();
    assert_eq!(
        run_plan_with(&p, &r, false, false, 1, &rt(&r)).unwrap_err(),
        "'steps'"
    );
}
