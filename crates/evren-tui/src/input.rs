//! Satır girdisi (reedline ya da düz stdin) ve ESC izleyicisi (Python `interrupt.py`).

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{BufRead, IsTerminal};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use reedline::{FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};
use tokio_util::sync::CancellationToken;

use crate::ui;

pub enum ReadResult {
    Line(String),
    Interrupted,
    Eof,
}

pub trait LineReader {
    fn read_line(&mut self, label: &str) -> ReadResult;
}

/// Uçbirim yoksa (boru, test): etiketi yazar, stdin'den satır okur.
pub struct PlainReader;

impl LineReader for PlainReader {
    fn read_line(&mut self, label: &str) -> ReadResult {
        print!("{label}");
        ui::flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => ReadResult::Eof,
            Ok(_) => ReadResult::Line(line.trim_end_matches(['\r', '\n']).to_string()),
        }
    }
}

struct LabelPrompt(String);

impl Prompt for LabelPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.0)
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("... ")
    }
    fn render_prompt_history_search_indicator(&self, _search: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed("(geçmiş) ")
    }
}

/// Etkileşimli giriş; geçmiş `.evren/history.txt`.
pub struct ReedlineReader {
    editor: Reedline,
}

impl ReedlineReader {
    pub fn new(history: &Path) -> Self {
        let mut editor = Reedline::create();
        if let Ok(h) = FileBackedHistory::with_file(1000, history.to_path_buf()) {
            editor = editor.with_history(Box::new(h));
        }
        Self { editor }
    }
}

impl LineReader for ReedlineReader {
    fn read_line(&mut self, label: &str) -> ReadResult {
        match self.editor.read_line(&LabelPrompt(label.to_string())) {
            Ok(Signal::Success(line)) => ReadResult::Line(line),
            Ok(Signal::CtrlC) => ReadResult::Interrupted,
            Ok(Signal::CtrlD) | Err(_) => ReadResult::Eof,
            Ok(_) => ReadResult::Interrupted,
        }
    }
}

/// Uçbirime uygun okuyucuyu seçer.
pub fn make_reader(history: &Path) -> Box<dyn LineReader + Send> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Box::new(ReedlineReader::new(history))
    } else {
        Box::new(PlainReader)
    }
}

/// Onay sorusu sırasında ESC izleyicisini durdurmak için paylaşılan bayrak.
#[derive(Clone, Default)]
pub struct WatchPause(Arc<AtomicBool>);

impl WatchPause {
    pub fn set(&self, paused: bool) {
        self.0.store(paused, Ordering::SeqCst);
    }
    fn get(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Tur sırasında yazılan satırlar (Python `QUEUE`); tur bitince sırayla çalışır.
pub type InputQueue = Arc<Mutex<VecDeque<String>>>;

/// Çalışan turda ESC'yi izler; ESC → iptal, Enter → yazılanı kuyruğa ekler.
pub struct EscWatcher {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EscWatcher {
    /// Uçbirim yoksa izleyici başlamaz (Ctrl+C yine çalışır).
    pub fn start(cancel: CancellationToken, pause: WatchPause, queue: InputQueue) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        if !std::io::stdin().is_terminal() {
            return Self { stop, handle: None };
        }
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut buffer = String::new();
            while !flag.load(Ordering::SeqCst) {
                if pause.get() {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                if !matches!(event::poll(Duration::from_millis(50)), Ok(true)) || pause.get() {
                    continue;
                }
                let Ok(Event::Key(key)) = event::read() else {
                    continue;
                };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Esc => {
                        if !cancel.is_cancelled() {
                            ui::print_warning("ESC: işlem durduruluyor...");
                        }
                        cancel.cancel();
                    }
                    KeyCode::Enter => {
                        let text = std::mem::take(&mut buffer);
                        if !text.trim().is_empty() {
                            if let Ok(mut q) = queue.lock() {
                                q.push_back(text.trim().to_string());
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                    }
                    KeyCode::Char(c) => buffer.push(c),
                    _ => {}
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
