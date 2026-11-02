# Faz 3 devir notu

`docs/faz3-prompt.md` adımlarının durumu ve kalan iş. Bağlam yarıyı geçtiği için oturum adım 6'nın ilk yarısından sonra durdu.

## Yapılanlar

- **3a** (`626ea02`): `prompt_frame` portu, oturum yardımcıları, TUI bağımlılıkları (reedline 0.52, termimad 0.35, crossterm 0.29).
- **3b** (adım 1 + 2, `f7dea98`, `cc58ecc`): `crates/evren-tui`
  - `events.rs`: `AgentEvent` tüketicisi. Onay `Decision::{Allow, Deny, Always}`; `Always` son `ToolCall`'ın kuralını `.evren/settings.local.json`'a yazar, tur sonunda `reload_settings()`. Uçbirim yoksa onay `confirm_headless`.
  - `input.rs`: `LineReader` (reedline + `.evren/history.txt` ya da düz stdin). `EscWatcher`: ESC → iptal, Enter → kuyruk. Ctrl+C turda iptal.
  - `repl.rs`: açılış (Bağlam satırı, en iyi anahtar, şartlar, kota, banner, `--continue`), `Brief > ` / `Düzeltme > `, `devam`, `execute_followup`, brief → `apply_frame`.
  - Argümansız `evren` REPL'i açar. `tests/compat` `test_repl_startup` geçiyor.
- **3c** (adım 3, `0c59c25`): açılışta CHANGELOG sürüm notları (en çok 4, 56 karakter), `/version` en çok 20 not.
- **3d** (adım 4, `e8232b7`): `markdown.rs` termimad akışı (kod çiti ve tablo bütün basılır; düz dalda işaretler silinir), rich araç/diff/düşünme satırları, şema varsayılanı gizleme, `EVREN_UI_THEME` paleti.
- **3e** (adım 5, `1395dcc`): `evren-agent/src/paste.rs` (`paste.py` tam portu, `.evren/tmp/paste-N.txt`), `AgentSession::compact_user_text` (vault + sıkıştırma; brief ve `execute_followup` kullanır). `evren-tui/src/shell.rs` `!komut` (600 sn, Ctrl+C → 130, `stream_with_tree_kill_until`).
- **3f** (adım 6'nın ilk yarısı, `fe9f985`):
  - `evren-tui/src/commands.rs`: `COMMAND_ALIASES` (Python `commands.py` ile aynı), `canonical`, `first_token`, `is_command`, `is_unknown_command`, `is_command_input` (`/ask x`, `/plan x` ajana; yol/derleme çıktısı/çok satır görev metni), `suggest_command` (difflib `SequenceMatcher.ratio` + `get_close_matches` portu, aynı harfli aday önce), `unknown_command_message` ("Bunu mu demek istediniz: X?").
  - `evren-tui/src/handlers.rs`: `/effort /models /quota(/status) /terms /tokens /steps` (metinler `repl.py` ile aynı).
  - `repl.rs` girdi yönlendirmesi `starts_with('/')` yerine `is_command_input`; komut eşleşmesi kanonik ad üzerinden.

## Kalan iş (sırayla, her biri ayrı commit)

1. **Adım 6, ikinci yarı**: `repl.rs` `PENDING_COMMANDS` içindekiler. Python yerleri (`repl.py` satır, uzunluk):
   - `/context` 1473, `/compact` 1477, `/rewind` 1481, `/doctor` 1485, `/rol` 1489, `/commit` 1461, `/review` 1465, `/init` 1469 → hepsi `extras.py handle_*`.
   - `/files` 1344, `/add` 1355, `/drop` 1363, `/scan` 1335, `/run` 1399.
   - `/memory` 1199 (40 satır), `/language` 1319, `/resume` 1297.
   - `/keys` → `_handle_keys_command`, `/debug` → `_handle_debug_command`, `/docs`.
   - `/clear` önce `clear_checkpoint` çağırmalı (Python), şu an yalnız `clear_history`.
   - Özel komutlar `.evren/commands/*.md` (`commands.py`): `is_command`/`is_unknown_command` dinamik kümeyi de saymalı (Python `DYNAMIC_COMMANDS`); döngüde `cmd in custom and (cmd not in table or cmd == "/clear")`.
   - `test_paste_input.py` `PromptFrameRoutingTest` vakaları adım 7 ile birlikte.
2. **Adım 7**: `--rol/--cerceve/--brief/--kisit/--cikti` → `PromptFrame`; `collect_prompt_frame` form akışı (`EVREN_FORM_FIELDS`, `EVREN_CLARIFY_ROUNDS`, `EVREN_FRAME_SUGGEST`). `test_prompt_frame.py` 18 vaka.
3. **Diğer**: plan kipinde checkpoint sorusu, "Planı uygula? [e/H]"; `project_docs` portu; görsel yolu → `@` mention.
4. **Doğrulama**: `tests/compat` (`repl_startup`) yeniden; `target\release\evren.exe` etkileşimli elle deneme (`/help`, `/model`, onaylı dosya oluşturma, `/undo`, `/exit`, ESC iptali). Etkileşimli deneme henüz yapılmadı.

## Varsayımlar

- `cli_home` belgeleri Faz 4'e bırakıldı.
- Yapıştırma sıkıştırması `run_step` yerine oturumda (`compact_user_text`) yapılıyor.
- `/models` ve `/quota` sırasında Python'daki dönen bekleme göstergesi yok (TUI'de spinner yok); son çıktı aynı.
- `/quota` şartlar alanları Python `str()` biçiminde (`True/False/None`) basılıyor.
