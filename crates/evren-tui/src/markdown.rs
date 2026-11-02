//! Akan asistan yanıtının markdown gösterimi (Python `print_markdown_stream`,
//! `flush_markdown`).
//!
//! Uçbirimde (renkli) yanıt satır satır termimad ile biçimlenir; kod blokları
//! ve tablolar tamamlanınca bütün olarak basılır. Uçbirim değilse ya da renk
//! kapalıysa her tam satır markdown işaretleri silinerek hemen basılır.

use termimad::MadSkin;

use crate::ui::strip_markdown;

/// Seçili temaya göre termimad görünümü (`EVREN_UI_THEME`).
pub fn skin(theme: &str) -> MadSkin {
    if theme == "light" {
        MadSkin::default_light()
    } else {
        MadSkin::default_dark()
    }
}

/// Tek bir yanıtın akış durumu.
pub struct MarkdownStream {
    rich: bool,
    skin: MadSkin,
    /// Henüz satır sonu gelmemiş kısım.
    pending: String,
    /// Bütün basılacak blok (kod çiti ya da tablo).
    block: Vec<String>,
    in_fence: bool,
    active: bool,
}

impl MarkdownStream {
    pub fn new(rich: bool, theme: &str) -> Self {
        Self {
            rich,
            skin: skin(theme),
            pending: String::new(),
            block: Vec::new(),
            in_fence: false,
            active: false,
        }
    }

    /// Akış bitmemiş bir yanıt var mı (`markdown_stream_active`).
    pub fn active(&self) -> bool {
        self.active
    }

    /// Parçayı ekler; hemen basılacak metni döner.
    pub fn push(&mut self, chunk: &str) -> String {
        self.active = true;
        self.pending.push_str(chunk);
        let mut out = String::new();
        while let Some(pos) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']).to_string();
            out.push_str(&self.line(line));
        }
        out
    }

    /// Kalan kısmı biçimleyip döner ve durumu sıfırlar.
    pub fn finish(&mut self) -> String {
        if !self.active {
            return String::new();
        }
        let mut out = String::new();
        let rest = std::mem::take(&mut self.pending);
        if !rest.is_empty() {
            out.push_str(&self.line(rest));
        }
        out.push_str(&self.flush_block());
        self.in_fence = false;
        self.active = false;
        out
    }

    fn line(&mut self, line: String) -> String {
        if !self.rich {
            return format!("{}\n", strip_markdown(&line));
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            if self.in_fence {
                self.block.push(line);
                self.in_fence = false;
                return self.flush_block();
            }
            let out = self.flush_block();
            self.in_fence = true;
            self.block.push(line);
            return out;
        }
        if self.in_fence {
            self.block.push(line);
            return String::new();
        }
        if trimmed.starts_with('|') {
            self.block.push(line);
            return String::new();
        }
        let mut out = self.flush_block();
        out.push_str(&self.render(&line));
        out
    }

    fn flush_block(&mut self) -> String {
        if self.block.is_empty() {
            return String::new();
        }
        let text = std::mem::take(&mut self.block).join("\n");
        self.render(&text)
    }

    fn render(&self, text: &str) -> String {
        if text.trim().is_empty() {
            return "\n".to_string();
        }
        self.skin.term_text(text).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_mode_streams_line_by_line_without_marks() {
        let mut s = MarkdownStream::new(false, "dark");
        assert_eq!(s.push("**Başlık** satırı\nikinci "), "Başlık satırı\n");
        assert!(s.active());
        assert_eq!(s.push("`kod` parçası"), "");
        assert_eq!(s.finish(), "ikinci kod parçası\n");
        assert!(!s.active());
        assert_eq!(s.finish(), "");
    }

    #[test]
    fn rich_mode_renders_without_raw_marks() {
        let mut s = MarkdownStream::new(true, "dark");
        let mut out = String::new();
        for chunk in ["**Sonuç:** ", "testler ", "geçti.\n\n- madde"] {
            out.push_str(&s.push(chunk));
        }
        out.push_str(&s.finish());
        assert!(
            out.contains("Sonuç:") && out.contains("testler geçti."),
            "{out}"
        );
        assert!(!out.contains("**"), "{out}");
        assert!(out.contains("madde"));
    }

    #[test]
    fn rich_mode_buffers_code_fence_until_closed() {
        let mut s = MarkdownStream::new(true, "dark");
        assert_eq!(s.push("```rust\nlet a = 1;\n"), "");
        let out = s.push("```\n");
        assert!(out.contains("let a = 1;"), "{out}");
        let table = s.push("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(table, "");
        let out = s.finish();
        assert!(out.contains('1') && out.contains('2'), "{out}");
    }
}
