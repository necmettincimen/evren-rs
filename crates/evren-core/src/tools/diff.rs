//! Birleşik diff üretimi ve cerrahi bul/değiştir (Python `diff_viewer.py`).

use serde_json::Value;

use super::args::{py_repr_str, py_type_name};
use super::file_ops::{split_lines, split_lines_keepends};

/// Satır düzeyinde fark işlemi (indisler satır dizilerine göre).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOp {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// LCS tablosu için üst sınır (hücre); aşılırsa orta bölge tek blok sayılır.
const LCS_CELL_LIMIT: usize = 4_000_000;

/// Ortak baş/son atılıp orta bölgede LCS ile satır farkı hesaplar.
pub fn line_diff<T: PartialEq>(old: &[T], new: &[T]) -> Vec<LineOp> {
    let limit = old.len().min(new.len());
    let mut start = 0;
    while start < limit && old[start] == new[start] {
        start += 1;
    }
    let mut tail = 0;
    while tail < limit - start && old[old.len() - 1 - tail] == new[new.len() - 1 - tail] {
        tail += 1;
    }
    let mut ops: Vec<LineOp> = (0..start).map(|i| LineOp::Equal(i, i)).collect();
    let old_mid = &old[start..old.len() - tail];
    let new_mid = &new[start..new.len() - tail];
    let n = old_mid.len();
    let m = new_mid.len();
    if n == 0 || m == 0 || n.saturating_mul(m) > LCS_CELL_LIMIT {
        ops.extend((0..n).map(|i| LineOp::Delete(start + i)));
        ops.extend((0..m).map(|j| LineOp::Insert(start + j)));
    } else {
        let w = m + 1;
        let mut table = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i * w + j] = if old_mid[i] == new_mid[j] {
                    table[(i + 1) * w + j + 1] + 1
                } else {
                    table[(i + 1) * w + j].max(table[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if old_mid[i] == new_mid[j] {
                ops.push(LineOp::Equal(start + i, start + j));
                i += 1;
                j += 1;
            } else if table[(i + 1) * w + j] >= table[i * w + j + 1] {
                ops.push(LineOp::Delete(start + i));
                i += 1;
            } else {
                ops.push(LineOp::Insert(start + j));
                j += 1;
            }
        }
        ops.extend((i..n).map(|k| LineOp::Delete(start + k)));
        ops.extend((j..m).map(|k| LineOp::Insert(start + k)));
    }
    let old_len = old.len();
    let new_len = new.len();
    ops.extend((0..tail).map(|k| LineOp::Equal(old_len - tail + k, new_len - tail + k)));
    ops
}

/// Eklenen ve silinen satır sayısı (`unified_diff(n=0)` +/- sayımı).
pub fn count_changes(old: &str, new: &str) -> (usize, usize) {
    let old_lines = split_lines_keepends(old);
    let new_lines = split_lines_keepends(new);
    let mut added = 0;
    let mut removed = 0;
    for op in line_diff(&old_lines, &new_lines) {
        match op {
            LineOp::Delete(_) => removed += 1,
            LineOp::Insert(_) => added += 1,
            LineOp::Equal(..) => {}
        }
    }
    (added, removed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Equal,
    Replace,
    Delete,
    Insert,
}

type Opcode = (Tag, usize, usize, usize, usize);

fn opcodes(ops: &[LineOp]) -> Vec<Opcode> {
    let mut out: Vec<Opcode> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut k = 0;
    while k < ops.len() {
        match ops[k] {
            LineOp::Equal(a, b) => {
                let (i1, j1) = (a, b);
                let mut i2 = a + 1;
                let mut j2 = b + 1;
                k += 1;
                while let Some(LineOp::Equal(a2, b2)) = ops.get(k) {
                    i2 = a2 + 1;
                    j2 = b2 + 1;
                    k += 1;
                }
                out.push((Tag::Equal, i1, i2, j1, j2));
                i = i2;
                j = j2;
            }
            _ => {
                let (i1, j1) = (i, j);
                let mut i2 = i;
                let mut j2 = j;
                while let Some(op) = ops.get(k) {
                    match op {
                        LineOp::Delete(a) => i2 = a + 1,
                        LineOp::Insert(b) => j2 = b + 1,
                        LineOp::Equal(..) => break,
                    }
                    k += 1;
                }
                let tag = match (i2 > i1, j2 > j1) {
                    (true, true) => Tag::Replace,
                    (true, false) => Tag::Delete,
                    _ => Tag::Insert,
                };
                out.push((tag, i1, i2, j1, j2));
                i = i2;
                j = j2;
            }
        }
    }
    out
}

fn grouped_opcodes(mut codes: Vec<Opcode>, n: usize) -> Vec<Vec<Opcode>> {
    if codes.is_empty() {
        codes.push((Tag::Equal, 0, 1, 0, 1));
    }
    if let Some(first) = codes.first_mut() {
        if first.0 == Tag::Equal {
            let (t, i1, i2, j1, j2) = *first;
            *first = (
                t,
                i1.max(i2.saturating_sub(n)),
                i2,
                j1.max(j2.saturating_sub(n)),
                j2,
            );
        }
    }
    if let Some(last) = codes.last_mut() {
        if last.0 == Tag::Equal {
            let (t, i1, i2, j1, j2) = *last;
            *last = (t, i1, i2.min(i1 + n), j1, j2.min(j1 + n));
        }
    }
    let nn = n + n;
    let mut groups = Vec::new();
    let mut group: Vec<Opcode> = Vec::new();
    for (tag, mut i1, i2, mut j1, j2) in codes {
        if tag == Tag::Equal && i2 - i1 > nn {
            group.push((tag, i1, i2.min(i1 + n), j1, j2.min(j1 + n)));
            groups.push(std::mem::take(&mut group));
            i1 = i1.max(i2.saturating_sub(n));
            j1 = j1.max(j2.saturating_sub(n));
        }
        group.push((tag, i1, i2, j1, j2));
    }
    if !group.is_empty() && !(group.len() == 1 && group[0].0 == Tag::Equal) {
        groups.push(group);
    }
    groups
}

fn format_range(start: usize, stop: usize) -> String {
    let mut beginning = start + 1;
    let length = stop.saturating_sub(start);
    if length == 1 {
        return beginning.to_string();
    }
    if length == 0 {
        beginning = beginning.saturating_sub(1);
    }
    format!("{beginning},{length}")
}

/// `difflib.unified_diff(..., fromfile=a/{name}, tofile=b/{name}, n=3)` eşdeğeri.
pub fn generate_unified_diff(old_content: &str, new_content: &str, filename: &str) -> String {
    let old_lines = split_lines_keepends(old_content);
    let new_lines = split_lines_keepends(new_content);
    let ops = line_diff(&old_lines, &new_lines);
    let groups = grouped_opcodes(opcodes(&ops), 3);
    let mut out = String::new();
    for (gi, group) in groups.iter().enumerate() {
        if gi == 0 {
            out.push_str(&format!("--- a/{filename}\n+++ b/{filename}\n"));
        }
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            format_range(first.1, last.2),
            format_range(first.3, last.4)
        ));
        for &(tag, i1, i2, j1, j2) in group {
            match tag {
                Tag::Equal => {
                    for line in &old_lines[i1..i2] {
                        out.push(' ');
                        out.push_str(line);
                    }
                }
                Tag::Replace | Tag::Delete | Tag::Insert => {
                    if matches!(tag, Tag::Replace | Tag::Delete) {
                        for line in &old_lines[i1..i2] {
                            out.push('-');
                            out.push_str(line);
                        }
                    }
                    if matches!(tag, Tag::Replace | Tag::Insert) {
                        for line in &new_lines[j1..j2] {
                            out.push('+');
                            out.push_str(line);
                        }
                    }
                }
            }
        }
    }
    out
}

/// "Değişiklik zaten dosyada" sonucunun öneki (hata değildir).
pub const ALREADY_APPLIED: &str = "ZATEN UYGULANMIŞ";

fn already_applied(original_text: &str, target_snippet: &str, replacement_snippet: &str) -> bool {
    let replacement = replacement_snippet.replace("\r\n", "\n");
    let replacement = replacement.trim();
    if replacement.is_empty() {
        return false;
    }
    let text = original_text.replace("\r\n", "\n");
    let target = target_snippet.replace("\r\n", "\n");
    text.contains(replacement) && !text.contains(target.trim())
}

fn contextual_not_found_error(original_text: &str, target_snippet: &str) -> String {
    let target_lines = split_lines(target_snippet);
    let Some(first_raw) = target_lines.first() else {
        return "Değiştirilmek istenen kod parçası dosyada bulunamadı.".to_string();
    };
    let first = first_raw.trim();
    let orig_lines = split_lines(original_text);
    let mut best_idx: Option<usize> = None;
    let mut best_score = 0.0f64;
    for (i, line) in orig_lines.iter().enumerate() {
        let stripped = line.trim();
        if stripped.is_empty() || first.is_empty() {
            continue;
        }
        let common = stripped
            .chars()
            .zip(first.chars())
            .take_while(|(a, b)| a == b)
            .count();
        let score = common as f64 / stripped.chars().count().max(first.chars().count()) as f64;
        if score > best_score {
            best_score = score;
            best_idx = Some(i);
        }
    }
    if let Some(best) = best_idx.filter(|_| best_score > 0.5) {
        if orig_lines[best].trim() == first && target_lines.len() > 1 {
            for (offset, expected) in target_lines.iter().enumerate().skip(1) {
                let idx = best + offset;
                let actual = orig_lines.get(idx).copied().unwrap_or("<dosya sonu>");
                if actual != *expected {
                    return format!(
                        "Değiştirilmek istenen kod parçası dosyada birebir bulunamadı. İlk satır (satır {}) eşleşiyor; En yakın satır (satır {}) ilk farkın olduğu yer:\n  Dosyada : {}\n  Aranan  : {}\nHedef parçayı bu satırdan itibaren dosyadaki metinle birebir düzelt.",
                        best + 1,
                        idx + 1,
                        py_repr_str(actual),
                        py_repr_str(expected)
                    );
                }
            }
        }
        return format!(
            "Değiştirilmek istenen kod parçası dosyada birebir bulunamadı. En yakın satır (satır {}):\n  Dosyada : {}\n  Aranan  : {}\nGirinti/boşluk farkı olabilir; lütfen dosyanın güncel içeriğini okuyup satırları birebir kopyalayın.",
            best + 1,
            py_repr_str(orig_lines[best]),
            py_repr_str(first_raw)
        );
    }
    "Değiştirilmek istenen kod parçası dosyada bulunamadı. Lütfen dosyanın güncel içeriğini okuyup satırları birebir kontrol edin.".to_string()
}

fn leading_ws(line: &str) -> &str {
    let trimmed = line.trim_start_matches([' ', '\t']);
    &line[..line.len() - trimmed.len()]
}

fn indent_tolerant_replace(text: &str, target: &str, replacement: &str) -> Option<String> {
    let mut t_lines: Vec<&str> = target.split('\n').collect();
    while t_lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        t_lines.pop();
    }
    while t_lines
        .first()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        t_lines.remove(0);
    }
    if t_lines.is_empty() {
        return None;
    }
    let key: Vec<&str> = t_lines.iter().map(|l| l.trim()).collect();
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() < key.len() {
        return None;
    }
    let hits: Vec<usize> = (0..=lines.len() - key.len())
        .filter(|&i| {
            key.iter()
                .enumerate()
                .all(|(j, k)| lines[i + j].trim() == *k)
        })
        .collect();
    if hits.len() != 1 {
        return None;
    }
    let start = hits[0];
    let file_base = leading_ws(lines[start]);
    let target_base = leading_ws(t_lines[0]);
    let mut out: Vec<String> = lines[..start].iter().map(|s| s.to_string()).collect();
    for ln in replacement.split('\n') {
        let adjusted = if !ln.trim().is_empty() && ln.starts_with(target_base) {
            format!("{file_base}{}", &ln[target_base.len()..])
        } else if !ln.trim().is_empty() && target_base.is_empty() {
            format!("{file_base}{ln}")
        } else {
            ln.to_string()
        };
        out.push(adjusted);
    }
    out.extend(lines[start + key.len()..].iter().map(|s| s.to_string()));
    Some(out.join("\n"))
}

/// Değiştirme sonucu: `Ok(yeni metin)` ya da `Err(mesaj)`.
pub type ReplaceResult = Result<String, String>;

/// `target_snippet` → `replacement_snippet`; tam eşleşme, CRLF toleransı,
/// girinti toleransı ve "zaten uygulanmış" denetimi sırasıyla.
pub fn apply_replacement(
    original_text: &str,
    target_snippet: &str,
    replacement_snippet: &str,
    replace_all: bool,
) -> ReplaceResult {
    if target_snippet.is_empty() {
        return Err("Hedef değiştirilecek metin (target_snippet) boş olamaz.".to_string());
    }
    let count = original_text.matches(target_snippet).count();
    if count == 1 {
        return Ok(original_text.replacen(target_snippet, replacement_snippet, 1));
    } else if count > 1 {
        if replace_all {
            return Ok(original_text.replace(target_snippet, replacement_snippet));
        }
        return Err(format!(
            "Değiştirilmek istenen metin dosyada birden fazla kez ({count} kez) bulundu. Tümünü değiştirmek için 'all' bayrağını kullanın veya daha belirgin bir bağlam parçası belirtin."
        ));
    }
    let has_crlf = original_text.contains("\r\n");
    let restore = |s: String| if has_crlf { s.replace('\n', "\r\n") } else { s };
    let normalized_orig = original_text.replace("\r\n", "\n");
    let normalized_target = target_snippet.replace("\r\n", "\n");
    let normalized_replace = replacement_snippet.replace("\r\n", "\n");
    let count_norm = normalized_orig.matches(normalized_target.as_str()).count();
    if count_norm == 1 {
        return Ok(restore(normalized_orig.replacen(
            &normalized_target,
            &normalized_replace,
            1,
        )));
    } else if count_norm > 1 && replace_all {
        return Ok(restore(
            normalized_orig.replace(&normalized_target, &normalized_replace),
        ));
    }
    if let Some(fuzzy) =
        indent_tolerant_replace(&normalized_orig, &normalized_target, &normalized_replace)
    {
        return Ok(restore(fuzzy));
    }
    if already_applied(original_text, target_snippet, replacement_snippet) {
        return Err(format!(
            "{ALREADY_APPLIED}: replacement_snippet dosyada zaten var, hedef metin artık yok. Bu değişiklik daha önce uygulanmış; tekrar deneme, sıradaki işe geç."
        ));
    }
    Err(contextual_not_found_error(original_text, target_snippet))
}

/// Düzenleme listesini sırayla uygular (`apply_edits`).
pub fn apply_edits(original_text: &str, edits: &[Value]) -> ReplaceResult {
    if edits.is_empty() {
        return Err("Düzenleme listesi boş olamaz.".to_string());
    }
    let mut current = original_text.to_string();
    let mut already: Vec<usize> = Vec::new();
    for (idx0, edit) in edits.iter().enumerate() {
        let idx = idx0 + 1;
        let Value::Object(map) = edit else {
            return Err(format!(
                "Edit #{idx} bir nesne olmalı (target_snippet/replacement_snippet), gelen tip: {}.",
                py_type_name(edit)
            ));
        };
        let empty = Value::String(String::new());
        let target = map.get("target_snippet").unwrap_or(&empty);
        let replacement = map.get("replacement_snippet").unwrap_or(&empty);
        let (Some(target), Some(replacement)) = (target.as_str(), replacement.as_str()) else {
            return Err(format!(
                "Edit #{idx}: target_snippet ve replacement_snippet metin olmalı (gelen: {}/{}).",
                py_type_name(target),
                py_type_name(replacement)
            ));
        };
        let replace_all = map.get("all").map(super::args::is_truthy).unwrap_or(false);
        match apply_replacement(&current, target, replacement, replace_all) {
            Ok(new_text) => current = new_text,
            Err(err) if err.starts_with(ALREADY_APPLIED) => already.push(idx),
            Err(err) => return Err(format!("Edit #{idx} başarısız: {err}")),
        }
    }
    if !already.is_empty() && current == original_text {
        let numbers = already
            .iter()
            .map(|n| format!("#{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "{ALREADY_APPLIED}: düzenleme {numbers} dosyada zaten uygulanmış; dosya değişmedi. Tekrar deneme, sıradaki işe geç."
        ));
    }
    Ok(current)
}
