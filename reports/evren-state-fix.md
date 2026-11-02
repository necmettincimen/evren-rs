# agent adımı "değişiklik yok" düzeltmesi ve kurulu ikili incelemesi

Tarih: 2026-11-02 · Sürüm: 4.1.1

## 1. Hata ve düzeltme

`crates/evren-cli/src/plan/agent.rs` `state()` yalnız `HEAD` + `git status --porcelain`
kullanıyordu. Porcelain içerik taşımadığı için adımdan önce zaten kirli olan dosya (`??` ya da
` M`) ajan tarafından tekrar düzenlendiğinde önceki/sonraki durum aynı çıkıyor, "değişiklik yok"
sayılıp adım `retries` kadar tekrarlanıyordu (rapor dosyasına çift ekleme riski).

Düzeltme:
- `state()` artık şunları birleştirir: `HEAD`, porcelain, `git diff HEAD --no-ext-diff --binary`
  çıktısının SHA-256'sı ve izlenmeyen dosyaların içerik özeti
  (`git ls-files --others --exclude-standard -z` + `git hash-object --stdin-paths`, birleşik
  çıktının SHA-256'sı). Çıktıların kendisi değil hash'i tutulur.
- Zaman aşımı 60 sn (`git_t`). `status` başarısızsa `None` (eski davranış). HEAD'siz depoda
  `diff HEAD` hata verir; o durumda diff hash'i boş kalır, durum yine üretilir (eski davranışla aynı).
- `dirty()` ve diğer çağıranlar değişmedi.
- `sha2` (çalışma alanında zaten vardı) `evren-cli` bağımlılığına eklendi.

Testler (`crates/evren-cli/src/plan/agent/tests.rs`):
- `edit_of_preexisting_untracked_file_counts_as_change`: önceden izlenmeyen `notes.md`, ajan
  satır ekliyor → "değişiklik yok" yok, ajan tek kez çağrılıyor (`retries: 2` iken).
- `edit_of_preexisting_modified_file_counts_as_change`: commit'siz değişmiş `a.txt` tekrar
  değiştiriliyor → aynı.
- `agent_doing_nothing_is_no_change`: kirli depoda ajan hiçbir şey yapmıyor → "değişiklik yok".
- Mevcut `fixed_outcome_without_change_fails` geçmeye devam ediyor.
- `cargo test -p evren-cli plan`: 40 geçti.

## 2. Kurulu ikili neden eskiydi (yalnız inceleme)

Bulgular:
- PATH sırası: `C:\Users\msi\.evren-cli\bin\evren.exe` → `%LOCALAPPDATA%\evren\bin\evren.exe` →
  Python `Scripts\evren.exe`. `scripts/install.ps1` ilk ikisine de `target/release/evren.exe`
  kopyalar.
- `evren update` (Rust `crates/evren-tui/src/updater.rs`) yeni ikili indirmez: ikilinin derlendiği
  checkout'u (`D:\Github\evren-rs`, `CARGO_MANIFEST_DIR`'den) `git pull --ff-only origin main`
  ile çeker, sonra süreç kapanınca ayrı pencerede `scripts/install.ps1` çalıştırır (derle + kopyala).
  Yani kurulan, o anki yerel çalışma ağacından derlenen ikilidir.
- `D:\Github\evren-rs\.evren\logs\update.log` son kayıt:
  `18:31 git pull … Already up to date` → `SCHEDULE DEFERRED` → `18:33 DEFERRED INSTALL: EXIT 1`.
  Ertelenmiş kurulum başarısız olmuş, eski ikili yerinde kalmış. Kurulum penceresinin çıktısı
  günlüğe yazılmadığı için kesin neden kayıtlı değil; aynı dakikalarda paralel ajanlar aynı
  `target/` dizininde `cargo` çalıştırıyordu (derleme kilidi / yarım çalışma ağacı ile derleme
  hatası en olası neden — doğrulanmadı).
- "4.0.0" etiketi yanıltıcıydı: `plan` seçenekleri (`--dry-run`, `-w`, `--from`) 0a5911f'te
  (16:30) eklendi ama sürüm numarası 4.1.0 commit'ine (18:26) kadar 4.0.0 kaldı. Bu yüzden önceki
  4.0.0 kurulumu ile yeni kod aynı sürüm adını taşıyordu.
- İkinci olası neden, bu düzeltmenin kurulumunda yeniden görüldü: `target/release/evren.exe`
  doğrudan çalıştırılırken (ör. `target\release\evren.exe plan … --from 4`) `cargo build --release`
  ikiliyi silemiyor (`Erişim engellendi, os error 5`), `install.ps1` derleme adımında düşüyor.
  Ertelenmiş kurulumdaki EXIT 1 de büyük olasılıkla budur. Çözüm olarak kilitli exe yeniden
  adlandırıldı (`evren.exe.running-<pid>`; Windows çalışan exe'nin adını değiştirmeye izin verir),
  derleme ve `install.ps1 -NoBuild` geçti.
- Şu an: `C:\Users\msi\.evren-cli\bin\evren.exe` ve `%LOCALAPPDATA%\evren\bin\evren.exe`
  4.1.1; `evren plan --help` `--dry-run`, `--from`, `-w` gösteriyor.
- Uzun plan işlerini `target\release\evren.exe` yerine kurulu `evren` ile çalıştırmak bu
  çakışmayı önler.

Öneriler (uygulanmadı):
- `schedule_deferred_install` kurulum çıktısını da `update.log`'a yazsın (`Start-Transcript` ya da
  `*>> $log`), EXIT 1'in nedeni kaybolmasın.
- Güncelleme öncesi çalışma ağacı kirliyse uyarı verilsin (yerel değişikliklerle derleme).
