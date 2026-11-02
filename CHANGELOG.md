# Değişiklik Günlüğü (Changelog)

Bu projedeki tüm önemli değişiklikler bu dosyada belgelenir.

Format [Keep a Changelog](https://keepachangelog.com/tr/1.1.0/) standardına,
sürümleme ise [Semantic Versioning](https://semver.org/lang/tr/) (SemVer) kurallarına dayanır.

## [4.0.0] - 2026-11-02

### Eklendi
- EVREN CLI Rust ile yeniden yazıldı: tek `evren` ikilisi, Python gerekmez.
- Etkileşimli REPL: reedline girişi, `.evren/history.txt` geçmişi, ESC ile tur iptali.
- `evren --stdio` (protokol v1) ve başsız `evren -p` kipi Python sürümüyle uyumlu.
- Ajan döngüsü Tokio üzerinde; onay ve `ask_user` olay kanalından döner.
- Aynı `.evren/` dosya biçimleri: ayarlar, oturumlar, yedekler, kullanım günlüğü.

### Kapsam dışı (ilk sürüm)
- `vision_search`, MCP istemcisi, terminal kurulumu, güncelleyici ve `/team`.
