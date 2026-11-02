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

## Kalanlar (sırayla)

2. Prompt çerçevesi: `--rol/--cerceve/--brief/--kisit/--cikti` bayrakları →
   `evren-agent/src/prompt_frame.rs` (mevcut `PromptFrame` genişletilir).
   - `collect_prompt_frame` form akışı (`EVREN_FORM_FIELDS`, `EVREN_CLARIFY_ROUNDS`,
     `EVREN_FRAME_SUGGEST`).
   - Testler: `test_prompt_frame.py` (18) ve `PromptFrameRoutingTest`.
3. `agent.md`/changelog yönetilen blokları (`src/project_docs.py`):
   - İşaretler `<!-- evren:managed:start section=... -->` ve `docs_state.json`.
   - `evren docs refresh`, `/docs unmark`.
   - REPL açılış önyüklemesi: `repl.rs` içindeki "Bağlam: atlandı" satırı kalkar.
   - `EVREN_DOCS_*` ve plan kipi "Planı uygula? [e/H]".
4. Faz 4:
   - memory aracı (`memory_save`), `web_fetch`.
   - `plan_runner`/`plan_agent` (`evren plan`, `.evren/logs` biçimi).
   - Komutlar: skills, playbook, template, track/task/progress/checkpoint/blockers/check-in/decisions.
   - SSH araçları ve `/ssh`, `/iterate`, `output_summary`, `tool_gateway`, TOML rolleri.
5. Kapanış:
   - Sürüm 4.1.0.
   - Doğrulama: `cargo build --release`, `cargo test`, `cargo test -- --ignored`, clippy.
   - `scripts/install.ps1` ve `evren --version` kontrolü; CHANGELOG ve push.

## Notlar

- Python `/doctor` Python sürüm satırı yerine çalıştırılabilir yolunu basar.
- `/resume playbook` Faz 4'e kadar Faz 4 mesajı verir.
