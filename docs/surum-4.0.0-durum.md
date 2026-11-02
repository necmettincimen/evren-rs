# Sürüm 4.0.0 durumu (2026-11-02)

İlk kurulabilir Rust sürümü. Hedef: evren-sdlc ve evren-vscode'un kullandığı iki yüzey
(`evren --stdio`, `evren -p`) Python 3.12 ile aynı davransın. REPL ikincil.

## Çalışanlar

- **evren-sdlc**: `evren --stdio --role R --task-ref T -w W --yes [--root ad=yol] [--model M] [--continue]`.
  - Yerleşik roller ve JSON rol dosyaları, rol kapısı, test kotası, adım/token bütçesi.
  - Tekrar kırıcı, `repeat_blocked` (stdio `tool_result.summary` içinde), `STUCK`/`BÜTÇE`.
  - `turn_end.outcome`, `bad_role`/`bad_root` (çıkış 2).
  - `EVREN_TRACE_FILE`; trace `request` satırı Python ile aynı alanlar
    (`task_id, step, model, prompt_tokens, cached_tokens, completion_tokens, ms, finish_reason`),
    `task_start` satırında `role` ve `task_ref`.
  - Doğrulama: örnek depoda (`examples/make_sample_repo.py`) gerçek anahtarla analist turu
    `outcome=ok`, T-0001 → ready, tokens 36693, cached 22336; sahte sunucuyla `outcome=ok`.
- **evren-vscode**: eklentinin gönderdiği tüm komutlar (`prompt` + `mode`/`attachments`,
  `cancel`, `approval_response` + `edited_content`, `ask_user_response`, `set_model`, `set_mode`,
  `list_sessions`, `resume`, `new_session`, `list_models`, `quota`, `slash`) ve beklediği olaylar.
  - Doğrulama: `npm test` 29/29 (şema: bu depodaki `protocol/stdio-v1.schema.json`),
    `npm run test:compat` 2/2 (`EVREN_CLI` = Rust ikilisi, sahte model sunucusu).
  - Eklentide değişiklik gerekmedi; `cliPath` varsayılanı `evren` artık Rust ikilisini bulur.
- **`-p`**: düz ve `--output json`; JSON alanları `result, changed_files, tokens, exit_code,
  model, outcome, role, repeat_blocked`.
- **REPL**: açılış, `/help`, onaylı dosya yazımı, `/exit` (sahte sunucuyla denendi).
- **Python uyumluluk testi** (`tests/compat`, `cargo test -- --ignored`): 18/18, kahin olarak
  Python 3.12 (`EVREN_CLI_ROOT` = evren-cli `evren-sdlc-faz0` worktree'si). evren-cli master hâlâ
  3.11 olduğundan varsayılan `../evren-cli` ile headless JSON anahtar testi başarısız olur.

## Çalışmayanlar / eksikler

Bkz. `CHANGELOG.md` → "Sonraki sürüm". Özetle: kalan slash komutları, prompt çerçevesi,
`agent.md` yönetilen blokları, `plan_runner`, SSH, skills, memory, `web_fetch`, TOML rol
dosyaları, MCP, `vision_search`, güncelleyici, `/team`.

## Kurulum ve geri alma

```bash
powershell -ExecutionPolicy Bypass -File scripts/install.ps1
powershell -ExecutionPolicy Bypass -File scripts/uninstall.ps1
```

- Kurulan yerler: `%LOCALAPPDATA%\evren\bin\evren.exe` ve `%USERPROFILE%\.evren-cli\bin\evren.exe`
  (sistem `PATH`'inde önde). Eski Python başlatıcıları `evren.exe.bak-<tarih>` olarak yanlarında.
- Python sürümü: `evren-py` (`%LOCALAPPDATA%\evren\bin\evren-py.cmd`).

## Açık notlar

- `~/.evren-sdlc.toml` hâlâ `command = [python, -m, src.cli]` (faz0 worktree) gösteriyor; sdlc'nin
  Rust ikilisini kullanması için kullanıcı bu bloğu `command = ["evren"]` yapmalı (dosyanın kendi
  yorumu da bunu söylüyor). Bu sürümde dokunulmadı.
- `changed_files` Python gibi `git status` farkından gelir; git deposu olmayan çalışma dizininde boştur.
- Rol dosyaları yalnız JSON (TOML crate'i eklenmedi).

## Kalan iş (öncelik sırası)

1. Kalan REPL slash komutları ve özel komutlar (eski `docs/faz3-devir.md` adım 6 ikinci yarı).
2. Prompt çerçevesi ve form akışı (adım 7).
3. `agent.md` yönetilen blokları, `project_docs`, plan kipi checkpoint sorusu.
4. Faz 4/5: SSH, skills, memory, `web_fetch`, MCP, `vision_search`, güncelleyici.
