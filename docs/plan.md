# Prompt: evren-rs — EVREN CLI'ın Rust ile yeniden yazımı

> Bu metni yeni bir oturuma olduğu gibi ver. İki repo var: `D:\Github\evren-cli` (Python 3.11.0, mevcut motor ve **oracle**) ve yeni `D:\Github\evren-rs` (bu iş). Bu dosya 2026-11-01 tarihli envantere dayanır; çelişkide canlı Python kaynağı esastır.

---

## Rol ve hedef

Sen kıdemli bir Rust geliştiricisisin. EVREN CLI'ı (Türkçe otonom yazılım geliştirme asistanı, OpenAI uyumlu EVREN LLM API ile çalışır) Rust'ta yeniden yazacaksın. Hedef **davranış paritesi**, satır satır çeviri değil:

- Aynı `evren` komut satırı, aynı `.evren/` dosya biçimleri, aynı `--stdio` NDJSON protokolü (v1). VS Code eklentisi (`evren-vscode`) ve `evren-sdlc` ikiliyi değiştirince farkı anlamamalı.
- Python testleri ve senaryoları oracle olarak kullanılır; Rust ikilisi aynı senaryolarda aynı olayları üretmeli.
- Tek statik ikili, Windows öncelikli (cmd.exe varsayılan kabuk, Git Bash isteğe bağlı), Linux/macOS ikincil.

Kapsam dışı (ilk sürüm): `vision_search`, `mcp_client`, `terminal_setup` (Windows Terminal profili), `updater` (GitHub'dan kendini güncelleme), `agent_library`/`/team` çoklu ajan tanımları. Bunlar Faz 5'te değerlendirilir.

## Neden Rust

Python sürümünün ölçülen sorunları: açılış 1-2 sn (rich + prompt_toolkit + openai import), `find_files`/`search_code` büyük repoda yavaş, arka plan süreç ağacı yönetimi (job object) kırılgan, tek dosya 2.4k satırlık `agent.py`. Rust hedefi: açılış < 50 ms, tek ikili, `ignore` crate ile ripgrep hızında arama, tip güvenli protokol.

## Çalışma kuralları (token tasarrufu)

- Python kaynağını **modül modül, yalnız portlarken** oku; 24k satırı baştan sona okuma. Aşağıdaki envanter keşif turunu kaldırmak için var.
- Her faz sonunda `cargo build --release`, `cargo test`, `cargo clippy -- -D warnings` geçer ve commit atılır. Commit mesajı Türkçe, AI imzası yok.
- Soru sorma; makul varsayımı yap, son mesajda tek satırla belirt. Yalnız geri dönüşsüz adımda (silme, push) onay iste.
- Uzun komutları arka planda çalıştır, `| tail` ile oku.
- Türkçe kullanıcı metinleri Python'daki metinlerle **aynı** olsun (test ve alışkanlık uyumu); `src/ui.py` ve ilgili modülden kopyala.

---

## Faz 0: Ortam ve iskelet (ilk oturum, bununla başla)

1. Rust kurulu değil. Kur: `winget install Rustlang.Rustup` → `rustup default stable` (MSVC). Doğrula: `cargo --version`.
2. `D:\Github\evren-rs` oluştur, `git init`, MIT lisans, `.gitignore` (target/, .evren/, .env*).
3. Cargo workspace:

```
evren-rs/
  Cargo.toml            # [workspace] members = crates/*
  crates/
    evren-core/         # config, settings, file_ops, secret_vault, tools, agent loop, compaction, trace
    evren-api/          # OpenAI uyumlu istemci: SSE akışı, keep-alive filtresi, reasoning, kota başlıkları, anahtar havuzu, retry
    evren-protocol/     # stdio v1 tipleri (serde) + JSON şema uyum testi
    evren-tui/          # REPL: reedline girişi, renkli çıktı, diff görünümü, slash komutları
    evren-cli/          # bin "evren": clap, alt komutlar, --stdio, başsız kip
  tests/compat/         # Python oracle'ı çalıştıran uyumluluk testleri (aşağıda)
  protocol/stdio-v1.schema.json   # evren-cli'dan kopya; CI'da diff ile eşitlik kontrolü
```

4. Bağımlılıklar (sürümleri crates.io'dan güncel al): `tokio` (rt-multi-thread, process, io, signal), `tokio-util` (CancellationToken), `reqwest` (rustls-tls, stream, json), `eventsource-stream` ya da elle SSE ayrıştırma, `serde`/`serde_json`, `clap` (derive), `anyhow` + `thiserror`, `tracing` + `tracing-appender`, `ignore` + `globset` + `regex`, `similar` (diff), `reedline` (giriş), `crossterm` + `owo-colors`, `termimad` (markdown), `dirs`, `chrono`, `uuid`, `sha2`, `encoding_rs` (BOM/CRLF koruma), `windows-sys` (job object; yalnız `cfg(windows)`), `jsonschema` (dev). `syntect` ekleme (ikili şişer); vurgu gerekiyorsa Faz 4'te tartış.
5. `evren --version` ve `evren --stdio` → `ready` olayı yazıp EOF'ta 0 ile çıkan iskelet. Bu çalışınca Faz 0 biter, commit.

---

## Faz 1: Çekirdek — config, API istemcisi, araçlar (LLM'siz test edilebilir)

### 1a. Config ve ayarlar (`evren-core::config`, `settings`)
Python: `src/config.py` (684), `src/settings.py` (224), `src/secret_vault.py` (117).

- `.env` yükleme sırası: `<ws>/.env` → `~/.evren/.env` → ortam. Anahtarlar: `EVREN_API_KEY`, `EVREN_API_KEYS` (virgüllü havuz), `EVREN_BASE_URL` (varsayılan `https://evren-llmapi.ssyz.org.tr/v1`), `EVREN_DEFAULT_MODEL`, `EVREN_FALLBACK_MODEL`, `EVREN_FALLBACK_AFTER`, `EVREN_DEFAULT_MAX_TOKENS`, `EVREN_DEFAULT_TEMPERATURE`, `EVREN_AUTO_APPROVE`, `EVREN_MAX_STEPS`, `EVREN_COMPACT_RATIO`, `EVREN_COMPACT_TARGET`, `EVREN_CONTEXT_BUDGET`, `EVREN_SMART_COMPACT`, `EVREN_SUMMARY_MODEL`, `EVREN_UI_THEME`, `EVREN_REASONING_EFFORT`, `EVREN_KEY_MIN_REMAINING`, `EVREN_AUTO_ROUTE`, `EVREN_PLANNER_MODEL`, `EVREN_EXECUTOR_MODEL`, `EVREN_PLANNER_STEPS`, `EVREN_EXECUTOR_THINKING`, `EVREN_ROUTE_MIN_TOKENS`, `EVREN_CLEF_INTENT`, `EVREN_PASTE_MAX_LINES`, `EVREN_PASTE_MAX_CHARS`, `EVREN_TRACE`, `EVREN_CA_BUNDLE`, `EVREN_SSL_VERIFY`, `EVREN_GIT_BASH`, `EVREN_SHELL`. Varsayılanları `config.py`'den al.
- Ayar katmanları (sonraki kazanır, listeler birleşir): `~/.evren/settings.json` → `<ws>/.evren/settings.json` → `<ws>/.evren/settings.local.json`. İzin kuralları (`allow`/`deny`, `tool(subject)` biçimi), `pre_tool`/`post_tool`/`stop` hook'ları, kalıcı `rol`. `add_allow_rule` → `settings.local.json`.
- `SecretVault`: ortamdaki sırları toplar, çıktı/olay/oturum dosyasında maskeler. Tek global aktif vault.

### 1b. API istemcisi (`evren-api`)
Python: `src/api_client.py` (841), `src/api_key_pool.py` (392), `src/errors.py` (247), `src/terms.py` (199).

- OpenAI `chat/completions` akışlı ve akışsız; tool_calls birleştirme (index'e göre parça birleştirme), `reasoning` / `reasoning_content` ayrı akış, `: keep-alive` SSE satırlarını atla.
- Kota: `x-evren-daily-remaining-tokens` başlığı + `GET /v1/quota`; `_normalize_quota` eşdeğeri. Anahtar havuzu: düşük kotada anahtar değiştir (`EVREN_KEY_MIN_REMAINING`), `select_best_key`.
- Geçici hata retry (`_transient_backoff`, `_can_retry_transient`: akış başladıysa retry yok), reddedilen parametreyi düşürüp tekrar dene (`_drop_rejected_param`, ör. `reasoning_effort`).
- Şartlar: `GET /v1/terms/status`, kabul akışı. `EvrenApiError` → `thiserror` enum (kota, auth, terms, transient, model).
- Sahte sunucu testi: `evren-cli/scripts/fake_llm_server.py` ile aynı davranışı veren bir Rust test sunucusu yaz **ya da** testte Python sunucuyu alt süreç olarak başlat (ilk tercih: Python'u kullan, az kod).

### 1c. Dosya ve süreç araçları (`evren-core::tools`)
Python: `src/tools.py` (1740), `src/file_ops.py` (369), `src/tool_args.py` (373), `src/tool_registry.py`, `src/background.py` (223), `src/proc_utils.py` (342), `src/apply_patch.py`, `src/clone_tree.py`, `src/diff_viewer.py`.

Araç şeması (adlar ve parametreler Python ile birebir, `TOOLS_SCHEMA`'dan kopyala):
`view_file`, `edit_file`, `write_file`, `list_directory`, `find_files`, `search_code`, `run_command`, `check_background`, `stop_background`, `create_plan`, `ssh_list_hosts`, `ssh_run`, `ssh_read_file`, `ssh_write_file`, `memory_save`, `ask_user`, `todo_write`, `web_fetch`, `delegate`, `apply_patch`, `clone_tree`. `_TOOL_NAME_ALIASES` da taşınır. Kipe göre gizleme: `tools_for_mode(normal|ask|plan, include_ssh)`.

Davranış kuralları:
- Çalışma alanı sınırı (`is_safe_path`, `read_roots` ek salt-okunur kökler), MSYS yolu → Windows dönüşümü, `PROTECTED_FILES`/`.env` yazılmaz.
- Yazmadan önce yedek: `.evren/backups/<rel>.<ts>` ; `rollback_last_backup`; görev başına değişen dosya listesi (`reset_task_changes`, `_record_original`) → `/rewind`.
- `safe_write_file` BOM ve CRLF'yi korur, `atomic_write_text`, `write_changed_lines`.
- `edit_file`: eşleşme yoksa açıklayıcı hata, diff özeti (`_diff_summary`), onay (`request_approval` → kind: edit|command|ssh|other).
- `find_files`/`search_code`: `ignore` crate (gitignore'a saygı), ikili dosya atla, bağlam satırları, büyük çıktı kırpma (`_truncate_output` eşik ve mesajı aynı).
- `run_command`: Windows'ta cmd.exe, `EVREN_SHELL`/`EVREN_GIT_BASH` ile bash; çok satırlı komut hatası (`MultilineShellCommand`); zaman aşımı; süreç ağacı öldürme (Windows job object, POSIX process group); `_pin_windows_filters` ve `_speed_grep` düzeltmeleri; `command_may_modify_files` ile onay kararı.
- Arka plan: `BackgroundManager` (başlat, `check_background{id, wait_seconds}`, `stop_background`), çıktı dosyaya yazılır, `/bg` listesi.
- Araç argüman normalizasyonu (`tool_args.py`): string→int/bool dönüşümü, eksik zorunlu alan hatası, JSON regex kaçışları (senaryo `json_regex_escapes.json`).

Bu fazın bitiş ölçütü: `evren-core` birim testleri Python'daki `test_tools.py`, `test_file_ops.py`, `test_tool_args.py`, `test_proc_utils.py`, `test_background.py`, `test_command_timeout.py`, `test_tool_output_truncate.py` vakalarını kapsar (vaka listesini bu dosyalardan çıkar, aynı girdi/çıktı).

---

## Faz 2: Ajan döngüsü ve `--stdio` (ilk kullanılabilir ikili)

Python: `src/agent.py` (2432), `src/stdio_server.py` (440), `src/modes.py`, `src/history_compactor.py` (413), `src/token_manager.py` (144), `src/trace.py` (531), `src/usage_log.py`, `src/sessions.py`, `src/todos.py`, `src/interrupt.py`, `src/summarizer.py`, `src/prompt_frame.py` (476), `src/session_context.py`, `src/intent.py`.

### 2a. `AgentSession` → Rust'ta `Agent` + `AgentEvent` kanalı
`agent.py`'yi parçala; tek dosya olmasın:
- `agent/loop.rs`: `run_step` / `_run_steps` / `_classify_response` / `_run_one_tool`. Adım bütçesi (`EVREN_MAX_STEPS`), duraklama bütçesi (`_stall_token_cap`, `_step_notes`), kesik tool_call (`_tool_arguments_complete`) çalıştırılmaz, ikinci ardışık kesilme → "görev tamamlanmadı".
- `agent/guards.rs`: repeat guard (`_repeat_guard`, `_call_signature`, `_command_family`), okuma tekrarı (`_read_repeat`, dosya digest), listeleme görevi kısıtı (`is_listing_task`), SSH hedef notu (`ssh_target_note`: "bu host/localhost" yereldir).
- `agent/context.rs`: sistem istemi birleştirme (`_compose_system`: agent.md + git bloğu + runtime env + rol + doğrulama notu `verification_note`/`detect_test_command`), bağlam dosyaları (`/add`, `/drop`), `token_report`.
- `agent/compaction.rs`: `HistoryCompactor` (araç çıktılarını stub'a indirgeme, `collapse_task`, `deterministic_summary`, akıllı özet için `EVREN_SUMMARY_MODEL`), eşikler `compaction_limit`/`_context_budget`.
- `agent/route.rs`: planner/executor yönlendirme (`_should_route`, `_plan_phase`, `_plan_to_todos`, `execution_brief`), `delegate` alt ajanı (`_run_delegate`, özyineleme yok — senaryo `delegate_no_recursion.json`).
- `agent/trace.rs`: `.evren/logs/trace.jsonl` biçimi birebir (evren-sdlc bunu okur), `/debug` adım tablosu, `usage_log`.
- Olaylar: ajan UI'ye doğrudan yazmaz; `mpsc<AgentEvent>` yayar (`AssistantDelta`, `ReasoningDelta`, `ToolCall`, `ToolResult`, `FileChangeProposed`, `ApprovalRequest`, `AskUser`, `Todos`, `Usage`, `TurnEnd`, `Error`). TUI ve stdio bu kanalı tüketir. Onay/soru cevapları `oneshot` ile geri döner. İptal `CancellationToken`.

### 2b. Oturum ve durum dosyaları (biçim paritesi şart)
`.evren/sessions/<ts>.json` (maskeli geçmiş; `--continue`, `/resume`), `.evren/history.txt`, `.evren/logs/trace.jsonl`, `.evren/backups/`, `.evren/docs_state.json`, `.evren/iterations/`, `.evren/ssh_hosts.json`, `.evren/tmp/`. Python'un ürettiği dosyayı Rust okuyabilmeli ve tersi.

### 2c. `evren --stdio` (`evren-protocol` + `evren-cli::stdio`)
Kaynak sözleşme: `evren-cli/docs/stdio-protocol.md`, `evren-cli/protocol/stdio-v1.schema.json`.
- Komutlar: `prompt{text, mode?, attachments?}`, `cancel`, `approval_response{request_id, decision: allow|deny|always, edited_content?}`, `ask_user_response`, `set_model`, `set_mode`, `list_sessions`, `resume`, `new_session`, `list_models`, `quota`, `slash{command, args?}` (güvenli alt küme: model, mode, new, clear, resume, sessions, models, quota, tokens, todos).
- Olaylar: `ready`, `turn_start`, `assistant_delta`, `reasoning_delta`, `tool_call`, `tool_result`, `file_change_proposed`, `approval_request`, `ask_user`, `todos`, `usage`, `turn_end{result, changed_files, exit_code}`, `error{code}`, `model_changed`, `mode_changed`, `sessions`, `session_loaded`, `models`, `quota`.
- Sıra garantileri ve `busy` kuralı dokümandaki gibi. stdout yalnız protokol, her şey stderr. Olay `id` biçimi `s<N>`. Bilinmeyen alan yok sayılır.
- Hata kodları enum olarak `evren-protocol`'de; şema dosyasındaki `x-error-codes` ile test eşitler.

### 2d. Başsız kip
`evren -p "<görev>" --output json --max-steps N --allowed-tools edit_file,run_command --yes [--continue] [--workspace]`. Çıkış kodu ve JSON çıktısı Python ile aynı; `evren-sdlc` ve `plan_agent` bunu çağırır.

### Faz 2 bitiş ölçütü — uyumluluk testi
`tests/compat/` içinde bir Rust test: `evren-cli/scripts/fake_llm_server.py`'yi başlatır, `evren-cli/tests/scenarios/*.json` senaryolarını hem Python (`python -m src.cli --stdio`) hem Rust ikilisine besler, olay dizilerini normalize edip (id, zaman, yol) karşılaştırır; ayrıca her olayı `stdio-v1.schema.json` ile doğrular. `test_stdio_compat.py` ve `test_stdio_protocol.py`'nin ne kontrol ettiğini oku, aynı kontrolleri yap. Bu geçince VS Code eklentisi Rust ikilisine yönlendirilip elle denenir.

---

## Faz 3: REPL / TUI (`evren-tui`)

Python: `src/repl.py` (1724), `src/ui.py` (663), `src/cli_home.py` (429), `src/paste.py`, `src/shell_mode.py`, `src/commands.py`, `src/language.py`, `src/version.py`, `src/diff_viewer.py`.

- Satır tabanlı REPL (tam ekran TUI değil): `reedline` ile geçmiş (`.evren/history.txt`), çok satırlı yapıştırma algısı (`paste.py` eşikleri), `!` kabuk kipi (`shell_mode.py`), ESC ile iptal (`interrupt.py`, `_check_esc`).
- Açılış banner'ı: sürüm, model, çalışma alanı, `CHANGELOG.md`'den sürüm notu; `cli_home.py` ana ekranı.
- Çıktı: markdown (termimad), araç çağrısı satırları, renkli diff (`diff_viewer.py`: yeşil `+` / kırmızı `-`, tablo), reasoning ayrı renkte canlı akış, görev özeti tablosu (`_print_task_summary`), `EVREN_UI_THEME`.
- Slash komutları — tam liste `evren-cli/commands.md`'de. Öncelik sırası:
  1. Çekirdek: `/help /version /model /models /mode /effort /quota /tokens /context /compact /clear /exit /files /add /drop /undo /rewind /auto /run /bg /todos /continue /execute /debug /steps /keys /terms /doctor /rol /scan /commit /review /init /docs /memory /language /resume`.
  2. İçerik dosyalarına dayalı: `/skills /skill /apply /playbook /diagnose /template /track /task /progress /checkpoint /blockers /check-in /decisions` (Faz 4 modülleri gelince).
  3. `/ssh`, `/iterate`, `/agent /agents /team`, `/update` (Faz 4-5).
- `prompt_frame.py`: `--rol/--cerceve/--brief/--kisit/--cikti` alanları ve form akışı (`collect_prompt_frame`, `is_execution_followup`, "devam" algısı). Sistem/kullanıcı içeriği birleştirme birebir.
- Özel komutlar: `.evren/commands/*.md` (`commands.py`).

Bitiş ölçütü: `test_repl_startup.py`, `test_paste_input.py`, `test_shell_mode.py`, `test_ui.py`, `test_prompt_frame.py` vakaları Rust testine taşınmış; `repl_startup.json` senaryosu geçer.

---

## Faz 4: Üst katman özellikleri

Her biri bağımsız modül, sırayla ve ayrı commit'lerle:

| Özellik | Python kaynağı | Not |
|---|---|---|
| Proje belgeleri (agent.md / changelog yönetilen bloklar) | `project_docs.py` (1124), `docs_merge.py`, `project_scanner.py` | `<!-- evren:managed:start section=... -->` birebir; `evren docs refresh/unmark/path`; `docs_state.json`; kill switch senaryosu |
| Plan çalıştırıcı | `plan_runner.py` (336), `plan_ops_remote.py` | `evren plan <json> --dry-run --allow-push --from N`; adım tipleri `parallel`, `push`, shell bash/cmd |
| Plan ajanları (worktree paralel) | `plan_agent.py` (607) | `_quarantine_main`, `_merge_back`, `report_has_evidence`, `report_ready_for_test`, `_parallel_cap`/`_agent_slot` dosya kilidi; doğrulama kapısı KANIT + GEÇTİ |
| İterasyon | `iteration.py` (642), `workflow_state.py` | `evren iterate --rounds --check --item-steps --resume --read-root --status`; `.evren/iterations/` |
| SSH | `ssh_hosts.py`, `ssh_ops.py` (364), `remote_presets.py`, `scripts/import_ssh_hosts.py` | `ssh_hosts.json`, denetim günlüğü, uzun çıktı spill, `evren ssh` alt komutları |
| Beceri/playbook/şablon/tanı | `skills.py`, `playbooks.py`, `templates.py`, `diagnostics.py`, `tracking.py`, `extras.py` (608) | Yerleşik içerik Python'dan kopyalanır; kullanıcı dosyaları `~/.evren/skills/` |
| Hafıza | `memory.py` | `memory_save` aracı, `/memory` |
| Web | `web_fetch.py` | `web_fetch` aracı; HTML → metin, boyut sınırı |
| Çıktı özeti / tokenlar | `output_summary.py`, `token_manager.py`, `file_cache.py` | Token tahmini sezgisel (tiktoken yok) — Python'daki formülü kopyala |
| Araç geçidi | `tool_gateway.py`, `tool_ext.py` | Dış araç uzantıları |
| Model listesi / rota | `models.py` | Model adları, `floor_output_tokens` tablosu |
| Yerleşik ajan tanımları | `src/agents/*.md` | `include_str!` ile gömülür |

---

## Faz 5: Dağıtım ve geçiş

- `cargo dist` ya da GitHub Actions matrisi: Windows x64 (MSVC), Linux x64 (musl), macOS arm64. Release etiketinde ikili + `stdio-v1.schema.json`.
- `scripts/install.ps1` / `install.sh` eşdeğeri: ikiliyi `~/.evren/bin`'e koyar, PATH'e ekler. Geçiş döneminde Python `evren` ile çakışmaması için ikili adı kurulumda `evren` kalır ama önce `evren-rs` olarak denenir; kullanıcı onayıyla ad değişir.
- CI: `cargo fmt --check`, `clippy -D warnings`, `cargo test`, uyumluluk testi (Python 3.12 + fake server), Ubuntu + Windows.
- Kapsam dışı bırakılanlar (`updater`, `terminal_setup`, `vision`, `mcp`) için karar: ihtiyaç varsa port, yoksa README'ye "Rust sürümünde yok" notu.
- Bitti ölçütü: VS Code eklentisi ve `evren-sdlc` bir hafta Rust ikilisiyle çalışır, Python sürümü arşivlenir.

---

## Mimari kararlar (değiştireceksen gerekçe yaz)

1. **Async çekirdek, senkron araçlar.** Tokio; LLM akışı async, dosya/süreç araçları `spawn_blocking`. İptal her yerde `CancellationToken`.
2. **Ajan UI bilmez.** Tek `AgentEvent` kanalı; TUI, stdio ve başsız kip üç ayrı tüketici. Python'daki `ui.print_*` çağrılarını ajana koyma.
3. **Hata tipi tek enum** (`EvrenError`): api, io, boundary, approval_denied, cancelled, budget. stdio hata kodlarına `From` dönüşümü.
4. **Dosya biçimleri sözleşmedir.** `.evren/` altındaki her JSON için serde struct + Python'un ürettiği örnek dosyayla round-trip testi.
5. **Windows ayrıntıları ayrı modülde** (`platform/windows.rs`, `platform/unix.rs`): job object, cmd.exe tırnaklama, MSYS yolu, BOM/CRLF.
6. **Metinler tek yerde** (`texts.rs`), Türkçe; ileride `language.py` eşdeğeri için `tr`/`en` anahtarı.
7. **Panik yok**: `unwrap` yalnız testte; clippy `unwrap_used` deny.

## İlk oturumun teslimatı

Faz 0 tamam + Faz 1a (config/settings/vault) + Faz 1b'nin akışsız `chat_complete` ve `list_models` kısmı. `evren models` ve `evren chat "merhaba"` gerçek API'ye karşı çalışır (anahtar `.env`'den). Son mesajda: yapılan, doğrulanan (komut çıktısı), açık kalan, sonraki oturum için tek satır başlangıç.
