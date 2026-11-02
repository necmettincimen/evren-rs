//! `live_diff` adımı: canlıdaki Kubernetes/Argo nesnesini repodaki YAML ile
//! karşılaştırır (canlı ile repo farkı; elle `kubectl get -o yaml` + göz diff'i yerine).
//!
//! ```text
//! {"op": "live_diff", "file": "deploy/argo/wf.yaml",
//!  "live_cmd": "kubectl get workflowtemplate x -n argo -o yaml" | "live_ssh": {...ssh adımı...},
//!  "ignore": ["spec.templates[0].metadata"], "live_only": false, "fail_on_drift": false,
//!  "save": "reports/drift.txt"}
//! ```
//!
//! Sunucunun eklediği alanlar (status, resourceVersion, uid, managedFields,
//! last-applied-configuration ...) atılır. Varsayılan olarak yalnız canlıda olan
//! yollar (sunucu varsayılanları) sayılır ama listelenmez (`live_only: true` ile listelenir).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::remote::ssh_run;
use super::{
    first_chars, flag, req_str, resolve_in_root, run_capture, say, shell_command, str_of, Step,
};

const SHOWN: usize = 60;
const RUNTIME_PATHS: [&str; 9] = [
    "status",
    "metadata.resourceVersion",
    "metadata.uid",
    "metadata.creationTimestamp",
    "metadata.generation",
    "metadata.managedFields",
    "metadata.selfLink",
    "metadata.annotations.kubectl.kubernetes.io/last-applied-configuration",
    "metadata.namespace",
];

fn live_text(step: &Step, root: &Path) -> Result<String, String> {
    if let Some(s) = step.get("live_ssh").and_then(Value::as_object) {
        return ssh_run(s, root);
    }
    let cmd = req_str(step, "live_cmd")?;
    let mut c = shell_command(&cmd);
    c.current_dir(root);
    let r = run_capture(&mut c, None, Duration::from_secs(120))?;
    if r.code != 0 {
        return Err(format!(
            "live_cmd çıkış kodu {}: {}",
            r.code,
            first_chars(String::from_utf8_lossy(&r.stderr).trim(), 300)
        ));
    }
    Ok(String::from_utf8_lossy(&r.stdout).into_owned())
}

fn yaml_docs(text: &str) -> Result<Vec<Value>, String> {
    use serde::Deserialize as _;
    let mut out = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(text) {
        let v = serde_yaml::Value::deserialize(doc).map_err(|e| format!("YAML: {e}"))?;
        if v.is_null() {
            continue;
        }
        out.push(serde_json::to_value(v).map_err(|e| format!("YAML→JSON: {e}"))?);
    }
    Ok(out)
}

fn ident(v: &Value) -> (String, String) {
    let s = |p: &str| {
        v.pointer(p)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    (s("/kind"), s("/metadata/name"))
}

/// `a.b.c` yolunu siler; `kubectl.kubernetes.io/...` gibi noktalı anahtarlar için en uzun eşleşen anahtar seçilir.
fn remove_path(v: &mut Value, path: &str) {
    let Some(obj) = v.as_object_mut() else { return };
    if obj.remove(path).is_some() {
        return;
    }
    let key = obj
        .keys()
        .filter(|k| path.starts_with(&format!("{k}.")))
        .max_by_key(|k| k.len())
        .cloned();
    if let Some(k) = key {
        if let Some(child) = obj.get_mut(&k) {
            remove_path(child, &path[k.len() + 1..]);
            if child.as_object().is_some_and(|m| m.is_empty()) {
                obj.remove(&k);
            }
        }
    }
}

fn flatten(v: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, c) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(c, &p, out);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for (i, c) in a.iter().enumerate() {
                flatten(c, &format!("{prefix}[{i}]"), out);
            }
        }
        other => {
            out.insert(prefix.to_owned(), other.to_string());
        }
    }
}

/// Fark satırları (`~` değişen, `-` yalnız repoda, `+` yalnız canlıda) ve gizlenen canlı-yalnız sayısı.
pub fn compare(
    repo: &Value,
    live: &Value,
    ignore: &[String],
    live_only: bool,
) -> (Vec<String>, usize) {
    let strip = |v: &Value| {
        let mut v = v.clone();
        for p in RUNTIME_PATHS
            .iter()
            .copied()
            .map(str::to_owned)
            .chain(ignore.iter().cloned())
        {
            remove_path(&mut v, &p);
        }
        let mut flat = BTreeMap::new();
        flatten(&v, "", &mut flat);
        flat.retain(|k, _| !ignore.iter().any(|i| k.starts_with(i.as_str())));
        flat
    };
    let (r, l) = (strip(repo), strip(live));
    let mut lines = Vec::new();
    let mut hidden = 0;
    for (k, rv) in &r {
        match l.get(k) {
            Some(lv) if lv != rv => lines.push(format!(
                "~ {k}: repo={} canlı={}",
                first_chars(rv, 80),
                first_chars(lv, 80)
            )),
            Some(_) => {}
            None => lines.push(format!("- {k}: {} (yalnız repoda)", first_chars(rv, 80))),
        }
    }
    for (k, lv) in &l {
        if !r.contains_key(k) {
            if live_only {
                lines.push(format!("+ {k}: {} (yalnız canlıda)", first_chars(lv, 80)));
            } else {
                hidden += 1;
            }
        }
    }
    (lines, hidden)
}

pub fn op_live_diff(step: &Step, root: &Path, dry: bool) -> Result<String, String> {
    let file = req_str(step, "file")?;
    if dry {
        return Ok(format!("canlı ↔ {file} karşılaştırılacak"));
    }
    let repo_text = std::fs::read_to_string(resolve_in_root(root, &file)?)
        .map_err(|e| format!("{file}: {e}"))?;
    let mut live = yaml_docs(&live_text(step, root)?)?;
    let live = match live.len() {
        1 => live.remove(0),
        n => {
            return Err(format!(
                "canlı çıktıda {n} belge var; tek nesne getiren komut ver (-o yaml, tek ad)"
            ))
        }
    };
    let docs = yaml_docs(&repo_text)?;
    let want = ident(&live);
    let repo = match docs.iter().find(|d| ident(d) == want) {
        Some(d) => d,
        None if docs.len() == 1 => &docs[0],
        None => return Err(format!("{file} içinde {}/{} bulunamadı", want.0, want.1)),
    };
    let ignore: Vec<String> = step
        .get("ignore")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let (lines, hidden) = compare(repo, &live, &ignore, flag(step, "live_only", false));
    if let Some(rel) = str_of(step, "save").filter(|s| !s.is_empty()) {
        let path = resolve_in_root(root, &rel)?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, lines.join("\n")).map_err(|e| e.to_string())?;
    }
    for l in lines.iter().take(SHOWN) {
        say(&format!("  {l}"));
    }
    if lines.len() > SHOWN {
        say(&format!("  ... {} satır daha", lines.len() - SHOWN));
    }
    let note = if hidden > 0 {
        format!(" ({hidden} yalnız-canlı yol gizlendi: sunucu varsayılanı; görmek için live_only)")
    } else {
        String::new()
    };
    let what = format!("{}/{}", want.0, want.1);
    if lines.is_empty() {
        return Ok(format!("{what}: canlı ile repo aynı{note}"));
    }
    let msg = format!("{what}: {} fark{note}", lines.len());
    if flag(step, "fail_on_drift", false) {
        Err(msg)
    } else {
        Ok(msg)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn runtime_fields_ignored_and_drift_found() {
        let repo = &yaml_docs("kind: WorkflowTemplate\nmetadata:\n  name: x\nspec:\n  entrypoint: main\n  args: [a, b]\n").unwrap()[0];
        let live = &yaml_docs(
            "kind: WorkflowTemplate\nmetadata:\n  name: x\n  uid: u1\n  resourceVersion: '9'\n  annotations:\n    kubectl.kubernetes.io/last-applied-configuration: '{}'\nspec:\n  entrypoint: other\n  args: [a]\n  serviceAccountName: default\nstatus: {}\n",
        )
        .unwrap()[0];
        let (lines, hidden) = compare(repo, live, &[], false);
        assert_eq!(
            lines,
            vec![
                "- spec.args[1]: \"b\" (yalnız repoda)".to_owned(),
                "~ spec.entrypoint: repo=\"main\" canlı=\"other\"".to_owned(),
            ]
        );
        assert_eq!(hidden, 1);
        let (lines, _) = compare(
            repo,
            live,
            &["spec.args".into(), "spec.entrypoint".into()],
            false,
        );
        assert!(lines.is_empty());
    }
}
