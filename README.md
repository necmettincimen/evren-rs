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

Yeni bilgisayarda önce Rust araç zinciri (GNU hedefi), MinGW araçları (`dlltool` için WinLibs)
ve depo; sonra yeni terminal aç:

```bash
winget install --id Rustlang.Rustup -e
winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e
rustup default stable-x86_64-pc-windows-gnu
git clone https://github.com/necmettincimen/evren-rs.git
```

Kurulum ve sürüm yükseltme aynı komut (depo klasöründe):

```bash
powershell -ExecutionPolicy Bypass -File scripts/install.ps1
```

Betik şunları yapar:

1. `cargo build --release` (atlamak için `-NoBuild`).
2. `target\release\evren.exe` dosyasını `%LOCALAPPDATA%\evren\bin\evren.exe` konumuna, varsa
   `%USERPROFILE%\.evren-cli\bin\evren.exe` konumuna da kopyalar (eski Python başlatıcısı
   sistem `PATH`'inde önde durduğu için değiştirilir).
3. Yerine konan her Python başlatıcısını önce `evren.exe.bak-<tarih>` olarak yedekler.
4. Python sürümü kuruluysa `evren-py` adıyla bırakır (`%LOCALAPPDATA%\evren\bin\evren-py.cmd`).
5. `%LOCALAPPDATA%\evren\bin` kullanıcı `PATH`'inde yoksa başa ekler (yeni terminal gerekir).

Sürüm yükseltme: `git pull` ardından aynı betik; önceki Rust kurulumunun üzerine yazar.

Doğrulama (yeni terminalde):

```bash
where evren
evren --version
```

API anahtarı ve model: `evren` açıp `/keys` ile ekle ya da başka makinedeki
`%LOCALAPPDATA%\evren\bin\config.json` dosyasını kopyala (depoya koyma). evren-sdlc için
`~/.evren-sdlc.toml` içinde `command = ["evren"]`.

## Geri alma

```bash
powershell -ExecutionPolicy Bypass -File scripts/uninstall.ps1
```

Her konumdaki en son `evren.exe.bak-<tarih>` yedeğini geri koyar ve `evren-py.cmd`'yi siler.
Yedekler silinmez; elle de geri konabilir.

## Durum

Bu sürümde neyin çalıştığı ve kalan işler: [docs/surum-4.0.0-durum.md](docs/surum-4.0.0-durum.md).
