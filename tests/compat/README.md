# Uyumluluk testleri

Python `evren-cli` ile Rust `evren` ikilisini aynı sahte modelle sürer, stdio olaylarını
şemaya göre doğrular ve dizileri karşılaştırır; `-p` başsız kipinin JSON çıktısını da eşler.

```bash
cargo build -p evren-cli
python -m pytest tests/compat -q
```

Ortam: `EVREN_CLI_ROOT` (varsayılan `../evren-cli`), `EVREN_RS_BIN` (varsayılan `target/debug/evren[.exe]`).
Gereksinim: `pytest`, `jsonschema`, `git`. `cargo test -p evren-cli --test compat -- --ignored` aynı testi cargo'dan çalıştırır.
