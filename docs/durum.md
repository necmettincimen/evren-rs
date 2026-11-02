# 4.1.0 çalışma durumu

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

## Kalanlar (sırayla)

4. Faz 4:
   - Komutlar: skills, playbook, template, track/task/progress/checkpoint/blockers/check-in/decisions.
   - SSH araçları ve `/ssh`, `/iterate`, `output_summary`, `tool_gateway`, TOML rolleri.
5. Kapanış:
   - Sürüm 4.1.0.
   - Doğrulama: `cargo build --release`, `cargo test`, `cargo test -- --ignored`, clippy.
   - `scripts/install.ps1` ve `evren --version` kontrolü; CHANGELOG ve push.

## Notlar

- `report_ready_for_test` Rust'ta henüz çağrılmıyor (Python'da ClickUp akışı kullanır).

- Python `/doctor` Python sürüm satırı yerine çalıştırılabilir yolunu basar.
- `/resume playbook` Faz 4'e kadar Faz 4 mesajı verir.
- REPL'de `EVREN_FORM_FIELDS` kapalıyken boş Brief hiçbir şey yapmaz (Python son kalıbı yeniden
  çalıştırır).
- `agent.md` çalışma zamanı satırı Rust sürümünü yazar; proje haritası Cargo.toml ipuçlarını da
  tanır. `cli_home` belgeleri (`ensure_cli_docs`) ve `current_focus` açık blokajları Faz 4 takip
  modülüyle gelecek.
- `cargo test -- --ignored` (Python oracle) 3.12.0+ ister; `../evren-cli` `master` 3.11.0'da
  olduğundan 5 test düşer. 3.12.2 (`claude/evren-sdlc-faz0-ce3b75`) kopyasıyla
  `EVREN_CLI_ROOT=<kopya>` verilince 18/18 geçer.
