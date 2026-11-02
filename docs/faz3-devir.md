# Faz 3 devir notu

`docs/faz3-prompt.md` adımlarının durumu ve kalan iş. Bağlam yarıyı geçtiği için oturum burada durdu.

## Yapılanlar

- **3a** (`626ea02`): `prompt_frame` portu, oturum yardımcıları, TUI bağımlılıkları (reedline 0.52, termimad 0.35, crossterm 0.29).
- **3b** (adım 1 + 2): `crates/evren-tui`
  - `events.rs`: `AgentEvent` tüketicisi. Onay `Decision::{Allow, Deny, Always}`; `Always` son `ToolCall`'ın kuralını `add_allow_rule(rule_for(..))` ile `.evren/settings.local.json`'a yazar, tur sonunda `reload_settings()`. `ask_user` cevabı oneshot ile döner. `Prompter` trait'i testte sahtelenir. Uçbirim yoksa onay `confirm_headless` ile varsayılanı döner.
  - `input.rs`: `LineReader` (uçbirimde reedline + `.evren/history.txt`, değilse düz stdin ve etiket). `EscWatcher`: tur sırasında crossterm `poll(50ms)`; ESC → `CancellationToken::cancel`, Enter → yazılan satır kuyruğa (Python `QUEUE`). Onay sorusu sırasında `WatchPause` izleyiciyi durdurur. Ctrl+C turda `tokio::signal::ctrl_c` ile iptal.
  - `repl.rs`: açılış (Bağlam satırı, en iyi anahtarı sabitleme, şartlar, kota, banner, `FORM_INTRO`, `--continue`), `evren [model] (kip)` başlığı, `Brief > ` / kesintiden sonra `Düzeltme > `, `devam` → `/continue`, `is_execution_followup` → `execute_followup`, brief → `apply_frame(frame, "", true)` + "Kalıp API'ye ayrıldı: ...". Komutlar: `/exit /quit /q /help /h /? /version /ver /v /model /mode /todos /bg [stop id] /clear /undo /rollback /auto /continue /execute`. Faz 4 komutları tek "Faz 4'te" mesajı; çekirdek kümenin kalanları şimdilik `NOT_PORTED_YET`.
  - `ui.rs`: düz metin çıktı (`[*] [+] [!] [X]`, ANSI renk yalnız TTY'de, `NO_COLOR`), banner (Python düz dal), `_shorten_path`, `_config_line`, `format_tool_args`, renkli diff, çalışma özeti.
  - Argümansız `evren` REPL'i açar (`main.rs` `None` dalı).
  - `tests/compat/test_compat.py::test_repl_startup` (Python + Rust) geçiyor; paket 18/18.

## Kalan iş (sırayla, her biri ayrı commit)

1. **Banner sürüm notları (adım 3)**: `version.py get_changelog_notes` portu. evren-rs kökünde `CHANGELOG.md` yok; ya eklenir ya `include_str!` ile gömülür. Banner en çok 4 not, `_clean_release_note` (56 karakter); `/version` en çok 20 not, `\nv{ver} Sürüm Notları:` + `  • {note}`.
2. **Çıktı (adım 4)**: termimad ile markdown (asistan metni akış bittiğinde biçimlenmeli; şu an akış ham basılıyor), `strip_markdown`, düşünme paneli, görev özeti tablosu (`AgentEvent::Todos` şu an yok sayılıyor), `EVREN_UI_THEME` (`Config::ui_theme`, FORM_PALETTES light/dark). `test_ui.py` 11 vakası.
3. **Yapıştırma / kabuk / devam (adım 5)**: `paste.py` portu (`.evren/tmp/paste-N.txt`, HEAD/TAIL 15, önemli satır regex'i, `EVREN_PASTE_MAX_LINES/CHARS`); Rust `run_step` sıkıştırma yapmıyor, TUI tur öncesi uygulamalı. `!komut`: `evren_core::tools::command::command_argv` (pub) + `proc::stream_with_tree_kill`, 600 sn, çıktı `push_user_message` ile `[Kullanıcının kendi çalıştırdığı kabuk komutu — bilgi için, yanıt gerekmez]` biçiminde (SHELL_NOTE_CHARS 4000, vault mask). `test_paste_input.py` 18, `test_shell_mode.py` 5 vaka.
4. **Slash komutları (adım 6)**: `repl.rs` `PENDING_COMMANDS` listesindekiler; metinler `repl.py` `_COMMAND_TABLE` (~1536) ve `commands.md`. `/commit /review /init /context /compact /rewind /doctor /rol` → `extras.py` `handle_*`. `unknown_command_message` öneri (`suggest_command`) ekle. Özel komutlar `.evren/commands/*.md` (`commands.py`), tablo komutlarını ezmez (`/clear` hariç).
5. **Bayraklar ve form (adım 7)**: `--rol/--cerceve/--brief/--kisit/--cikti` → `PromptFrame`; `collect_prompt_frame` alan akışı (`EVREN_FORM_FIELDS`, `EVREN_CLARIFY_ROUNDS`, `EVREN_FRAME_SUGGEST`). `test_prompt_frame.py` 18 vaka.
6. **Diğer**: plan kipinde duraklayan görev için checkpoint sorusu, "Planı uygula? [e/H]" teklifi; `project_docs` (Bağlam önyüklemesi) portu yoksa Bağlam satırı "atlandı" der; görsel yolu → `@` mention, `expand_mentions` uyarıları.
7. **Elle doğrulama**: `target\release\evren.exe` etkileşimli uçbirimde (banner, `/help`, `/model`, onaylı dosya oluşturma, `a=hep izin ver`, ESC iptali, `/undo`, `/exit`). Borulu gerçek API denemesi yapıldı (dosya oluşturma + `/undo` + `/exit`), etkileşimli ESC/onay denenmedi.

## Varsayımlar

- Çıktı Python'un rich olmayan düz dalı biçiminde; rich biçimi adım 4'te termimad ile.
- `test_repl_startup` sahte sunucu istek sayısını saymıyor; "sahte model" yanıtının çıkmaması ile LLM isteği olmadığı denetleniyor.
