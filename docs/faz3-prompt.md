# Prompt: evren-rs Faz 3 — REPL / TUI (devam)

> Bu metni `D:\Github\evren-rs` içinde yeni bir oturuma olduğu gibi ver. Önceki Faz 3 oturumu yarıda durduruldu; aşağıdaki "Mevcut durum" o oturumun bıraktığı yerdir.

## Bağlam

EVREN CLI (`D:\Github\evren-cli`, Python 3.11.0) Rust'a taşınıyor. Bu repo `evren-rs`: Cargo workspace, toolchain `stable-x86_64-pc-windows-gnu`, cargo `~/.cargo/bin` altında. Faz 0-2 bitti ve doğrulandı (10 commit, `main`, push yok):

- `evren-core` config/settings/vault/araçlar, `evren-api` SSE istemci + anahtar havuzu, `evren-protocol` stdio v1 tipleri, `evren-agent` ajan döngüsü (`AgentEvent` kanalı yayar), `evren-cli` bin (`models`, `chat`, `terms`, `--stdio`, başsız `-p`), `tests/compat` Python oracle uyumluluk testi.
- `evren-tui` boş. Argümansız `evren` → `crates/evren-cli/src/main.rs` `not_ported()` "Bu komut henüz Rust sürümüne taşınmadı".

Plan `docs/plan.md`; yalnız "Faz 3" ve "Mimari kararlar" bölümlerini oku.

## Mevcut durum (commit edilmemiş, derleniyor, testler geçiyor)

```
 M Cargo.toml, Cargo.lock           # reedline, termimad, crossterm workspace'e eklendi
 M crates/evren-tui/Cargo.toml      # bağımlılıklar eklendi, kod yok
 M crates/evren-agent/src/{context,lib,session}.rs
 M crates/evren-api/src/client.rs
?? crates/evren-agent/src/prompt_frame.rs   # 249 satır: PromptFrame, is_execution_followup,
                                            # identity_block, compose_system, build_user_content, delivery_summary
```

İlk iş: `git diff` ile bu değişiklikleri gözden geçir, `cargo test` ve `cargo clippy --all-targets -- -D warnings` geçiyorsa "Faz 3a: prompt_frame portu ve TUI bağımlılıkları" diye commit at. Sonra REPL'e başla.

## Görev: argümansız `evren` Python REPL gibi açılır ve çalışır

Sıra ve her adım ayrı commit:

1. **Olay tüketicisi.** `crates/evren-cli` içindeki stdio sunucusu `AgentEvent` kanalını nasıl tüketiyorsa TUI de öyle tüketir (üçüncü tüketici). Ajana UI kodu koyma. Onay (`allow/deny/always`; `always` → `settings.local.json`) ve `ask_user` cevapları oneshot ile döner.
2. **REPL çekirdeği.** Satır tabanlı, tam ekran değil. `reedline` giriş, geçmiş `.evren/history.txt`, ESC ile çalışan turu iptal (CancellationToken; Python `interrupt.py`). Argümansız `evren` bunu açar; `not_ported` yalnız Faz 4 alt komutlarında kalır.
3. **Açılış.** Banner: sürüm, model, çalışma alanı, `CHANGELOG.md` sürüm notu. Kaynak `evren-cli/src/cli_home.py`, `ui.py`, `version.py`.
4. **Çıktı.** Markdown (`termimad`), araç çağrısı satırları, reasoning ayrı renkte canlı akış, renkli diff (`diff_viewer.py`), görev özeti tablosu, `EVREN_UI_THEME`.
5. **Yapıştırma ve kabuk.** Çok satırlı yapıştırma algısı (`paste.py`, `EVREN_PASTE_MAX_LINES/CHARS`), `!` kabuk kipi (`shell_mode.py`), "devam" algısı (`prompt_frame::is_execution_followup`, zaten portlu).
6. **Slash komutları.** Çekirdek küme: `/help /version /model /models /mode /effort /quota /tokens /context /compact /clear /exit /quit /files /add /drop /undo /rewind /auto /run /bg /todos /continue /execute /debug /steps /keys /terms /doctor /rol /scan /commit /review /init /docs /memory /language /resume`. Takma adlar ve metinler `evren-cli/src/repl.py` `_COMMAND_TABLE` (satır ~1536) ve `evren-cli/commands.md` ile aynı. Faz 4'e bağlı olanlar (`/skills /playbook /diagnose /template /track /task /progress /checkpoint /blockers /check-in /decisions /ssh /iterate /agent /agents /team /update`) tek bir "Faz 4'te" mesajı verir. Özel komutlar `.evren/commands/*.md` (`commands.py`).
7. **Bayraklar.** `--rol/--cerceve/--brief/--kisit/--cikti` → `PromptFrame`; `prompt_frame.py` form akışı (`collect_prompt_frame`).

## Kurallar

- Python kaynağı `D:\Github\evren-cli\src\`; yalnız portladığın modülü oku, `repl.py` (1724 satır) parça parça. Türkçe kullanıcı metinleri Python ile aynı.
- Her commit öncesi `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`. Commit mesajı Türkçe, AI imzası / Co-Authored-By yok. Push yapma.
- Soru sorma; makul varsayımı yap, sonda tek satırla belirt.
- Oturum büyürse (bağlam yarıyı geçince) kalan işi `docs/faz3-devir.md` dosyasına yaz, commit at ve dur.

## Bitiş ölçütü

- `evren-cli/tests/test_repl_startup.py`, `test_paste_input.py`, `test_shell_mode.py`, `test_ui.py`, `test_prompt_frame.py` vakaları Rust testine taşınmış.
- `evren-cli/tests/scenarios/repl_startup.json` senaryosu geçer (`tests/compat` harness'ine ekle).
- Elle: `target\release\evren.exe` aç → banner, `/help`, `/model`, küçük ajan görevi (dosya oluşturma + onay akışı), `/undo`, `/exit`.
- Son mesaj: yapılanlar, doğrulama komutlarının gerçek çıktısı, açık kalanlar, sonraki oturum için tek satır.
