# 4.2.1 durumu (tamamlandı)

- Boş Brief önceki kalıbı yeniden çalıştırır; `/doctor` kaynak sürümü uyarısı;
  `NOT_PORTED_YET` kaldırıldı. `evren --version` → `evren-cli 4.2.1`.

## 4.2.0 durumu (tamamlandı)

- MCP istemcisi (`evren-core` `tools::mcp`): `mcpServers` ayarı, tembel başlatma,
  `mcp__<sunucu>__<araç>` şemaları normal kipte, onaylı çağrı, REPL çıkışında kapatma.
  Test, test ikilisinin kendisini sahte MCP sunucusu olarak çalıştırır.
- `web_search` (`tools::ext`): `webSearch.url` ayarlıysa sunulur ve dispatch edilir.
- `evren --version` → `evren-cli 4.2.0`.

## 4.1.2 durumu (tamamlandı)

4.1.2 kapanan açıklar (4.1.1 üzerine):
- Güncelleyici: ertelenen `install.ps1` çıktısı `.evren/logs/update.log`'a yazılır; kilitli
  `target\release\evren.exe` `evren.exe.running-<pid>` adına çekilir (`scripts/install.ps1` de);
  kirli çalışma ağacı uyarısı (`c605eb6`).
- `ToolContext` sır kasası: komutlar kasadan açılır, çıktı maskeli kalır (`b720037`).
- `evren scan` uzantıları ilk görülme sırasıyla, Python `rglob` sırasında (`0efa2b3`).
- `vision.rs` API anahtar havuzu (`KeySource` özelliği; `evren-api` uygular) (`71bb183`).
- Kapanış: `cargo test`, `cargo test -- --ignored` (3.12.2 kopyasıyla 18/18), clippy temiz;
  `evren --version` → `evren-cli 4.1.2`.

## 4.1.0 durumu (tamamlandı)

Hedef: 4.0.0 üzerine Python sürümüyle parite; sürüm 4.1.0. Ayrıntılı 4.0.0 durumu:
`docs/surum-4.0.0-durum.md`.

## Bitenler

1. REPL slash komutları ve özel komutlar (commit `f2e0cf6`):
   - Komutlar: `/rol /commit /review /init /context /compact /rewind /doctor /keys /debug
     /memory /language /scan /files /add /drop /run /resume`.
   - `.evren/commands/*.md` özel komutları (`is_command`/`is_unknown_command` dinamik kümeyi sayar).
   - `/clear` önce `.evren/checkpoint.json` siler.
   - Yeni modüller: `evren-core` `memory`, `language`, `project_scan`; `evren-agent`
     `trace_report`; `evren-tui` `extras`.
   - Doğrulama: `cargo test` 200 geçti, `cargo clippy --all-targets -- -D warnings` temiz.

2. Prompt çerçevesi:
   - `evren-agent` `prompt_frame`: `collect_prompt_frame`, `prepare_frame` (netleştirme döngüsü,
     `submit_frame_guidance` aracıyla öneriler), `FrameOptions` (`EVREN_FORM_FIELDS`,
     `EVREN_CLARIFY_ROUNDS`, `EVREN_FRAME_SUGGEST`).
   - REPL Brief akışı; `evren chat`/`evren agent` kalıp bayrakları, `load_frame`, Git Bash
     `/ask` yolu düzeltmesi; `evren agent` tek seferlik görev (`evren-tui` `oneshot`).
   - Testler: `test_prompt_frame.py`, `PromptFrameRoutingTest`, palet kontrastı taşındı.

3. `agent.md`/changelog yönetilen blokları (`evren-core` `project_docs`, `docs_merge`):
   - `<!-- evren:managed:start section=... -->` işaretleri, `.evren/docs_state.json`, `EVREN_DOCS_*`
     ve `EVREN_AUTO_DOCS` anahtarları; ajan turunda belge eşitleme ve changelog girişi.
   - `evren docs [status|refresh|unmark|path]`, REPL `/docs`; açılışta "Bağlam: ..." özeti.
   - Plan kipi sonrası "Planı uygula? [e/H]"; netleştirme bağlamına proje özeti.
   - Testler: `test_project_docs.py` (13 test) taşındı.

4. Faz 4 (kısmi, commit `6f60e94`):
   - `memory_save` aracı (`memory::add_note`, global/proje kapsamı; metin kasada maskelenir).
   - `web_fetch` aracı (`evren-core` `tools/web_fetch`: reqwest blocking, HTML→metin, bayt/karakter
     sınırı, ikili içerik reddi, `ssl_verify`); URL kasadan açılır.
   - Doğrulama: `cargo test` 262 geçti, clippy temiz.

5. Faz 4c `evren plan` (`crates/evren-cli/src/plan/`: mod/agent/remote):
   - `evren plan <plan.json> [--dry-run] [--allow-push] [--from N] [-w]`; satır biçimleri Python
     ile aynı (`[i/n] op: ...`, `HATA` + `Devam için: ...`, `PLAN TAMAMLANDI`).
   - Adımlar: clone_tree, replace, write, run, commit, push (kilitli, Drone tetikleme/bekleme),
     parallel (`max_workers`, süreçler arası slotlar), agent (worktree, sözleşme, rapor/SONUÇ,
     verify kapısı, adım sınırında devam, öldürülen denemeyi sürdürme) ve uzak adımlar.
   - `AgentRunner` özelliği testte sahte çalıştırıcıyla değişir; `test_plan_agent.py` (26 test)
     ve çalıştırıcı adım testleri taşındı.
   - Doğrulama: `cargo test` 299 geçti, clippy temiz; kuru çalıştırma çıktısı Python ile birebir.

6. Faz 4 komutları (commit `50357ea`, `61ebb7b`, `60baf7e`):
   - `/iterate` + `evren iterate` (`evren-tui` `iteration`).
   - `tool_gateway` paritesi (`guards::check_tool_allowed`) ve TOML rol dosyaları.
   - Ajan kitaplığı (`evren-agent` `agent_library`), `/agent`, `/agents`, `/team`, `@ajan`;
     `delegate` tanımlı alt ajanlar.
   - `/update` ve `evren update` (`evren-tui` `updater`); `PHASE4_COMMANDS` kaldırıldı.

7. Kapanış: sürüm 4.1.0; `cargo build --release`, `cargo test`, clippy, `scripts/install.ps1`,
   `evren --version` → `evren 4.1.0`.

## Sonraki işler

- `report_ready_for_test` plan akışına bağlanmalı (ClickUp "teste hazır").
- `ensure_cli_docs` (`cli_home` belgeleri) ve `current_focus` açık blokajları.

## Notlar

- `report_ready_for_test` Rust'ta henüz çağrılmıyor (Python'da ClickUp akışı kullanır).
- `/doctor` Python sürüm satırı yerine çalıştırılabilir yolunu basar (Rust'ta Python yok);
  `pyproject` karşılaştırması yerine kaynak `Cargo.toml` sürümünü karşılaştırır.
- `agent.md` çalışma zamanı satırı Rust sürümünü yazar; proje haritası Cargo.toml ipuçlarını da
  tanır. `cli_home` belgeleri (`ensure_cli_docs`) ve `current_focus` açık blokajları Faz 4 takip
  modülüyle gelecek.
- `cargo test -- --ignored` (Python oracle) 3.12.0+ ister; `../evren-cli` `master` 3.11.0'da
  olduğundan 5 test düşer. 3.12.2 (`claude/evren-sdlc-faz0-ce3b75`) kopyasıyla
  `EVREN_CLI_ROOT=<kopya>` verilince 18/18 geçer.
- `/update` Rust kurulumuna uyarlandı: kaynak satırı `github.com/necmettincimen/evren-rs`, pip yerine
  `kaynak klonu`; Windows'ta pip yerine `scripts/install.ps1` ertelenir.
