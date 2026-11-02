# evren-rs

EVREN CLI'ın Rust ile yeniden yazımı. Tek `evren` ikilisi; Python gerekmez.
Python sürümü (`D:\Github\evren-cli`) ile aynı `.evren/` dosya biçimlerini, `evren --stdio`
protokolünü (v1, `protocol/stdio-v1.schema.json`) ve `evren -p` başsız kipini kullanır.

Kullananlar: evren-sdlc (`evren --stdio --role R --task-ref T -w W --yes`) ve
evren-vscode (`cliPath` ayarı, `evren --stdio --workspace W`).

## Derleme

GNU araç zinciri (`x86_64-pc-windows-gnu`) ve WinLibs MinGW `PATH`'te olmalı.

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

## Kurulum (Windows)

```bash
powershell -ExecutionPolicy Bypass -File scripts/install.ps1
```

Betik şunları yapar:

1. `cargo build --release` (atlamak için `-NoBuild`).
2. `target\release\evren.exe` dosyasını `%LOCALAPPDATA%\evren\bin\evren.exe` ve
   `%USERPROFILE%\.evren-cli\bin\evren.exe` konumlarına kopyalar. İkincisi sistem `PATH`'inde
   önde durduğu için değiştirilir; `PATH`'in kendisine dokunulmaz.
3. Yerine konan her Python başlatıcısını önce `evren.exe.bak-<tarih>` olarak yedekler.
4. Python sürümünü `evren-py` adıyla bırakır (`%LOCALAPPDATA%\evren\bin\evren-py.cmd`).

Doğrulama:

```bash
where evren
evren --version
evren-py --version
```

## Geri alma

```bash
powershell -ExecutionPolicy Bypass -File scripts/uninstall.ps1
```

Her konumdaki en son `evren.exe.bak-<tarih>` yedeğini geri koyar ve `evren-py.cmd`'yi siler.
Yedekler silinmez; elle de geri konabilir.

## Durum

Bu sürümde neyin çalıştığı ve kalan işler: [docs/surum-4.0.0-durum.md](docs/surum-4.0.0-durum.md).
