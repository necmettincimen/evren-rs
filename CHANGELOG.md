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
- evren-sdlc uyumu (Python 3.12 davranışı): `--role`/`--profile` (yerleşik analyst, developer,
  tester, shipper ve JSON rol dosyaları), `--task-ref`, `--root ad=yol`; rol araç/yol/komut kapısı,
  test kotası, adım ve token bütçesi; tekrar kırıcı (`EVREN_REPEAT_LIMITS`), yinelenen çağrı
  yanıtı, `STUCK`/`BÜTÇE` sonuçları.
- stdio: hatalı rol/kökte `bad_role`/`bad_root` satırı ve çıkış 2; `turn_end.outcome`.
- `-p --output json`: `outcome`, `role`, `repeat_blocked` alanları.
- Trace: `EVREN_TRACE_FILE`; `task_start` satırında `role` ve `task_ref`.
- Windows kurulumu: `scripts/install.ps1` ve `scripts/uninstall.ps1` (yedekli; Python sürümü
  `evren-py` olarak kalır).

### Sonraki sürüm
- Yapıldı (main): `memory_save` ve `web_fetch` araçları.
- Yapıldı (main, 4.1.0'a girecek): `/context`, `/compact`, `/rewind`, `/doctor`, `/rol`,
  `/commit`, `/review`, `/init`, `/files`, `/add`, `/drop`, `/scan`, `/run`, `/memory`,
  `/language`, `/resume`, `/keys`, `/debug` ve `.evren/commands/*.md` özel komutları;
  `/clear` checkpoint'i siler.
- Yapıldı (main): `agent.md`/`changelog.md` yönetilen blokları (`.evren/docs_state.json`),
  `evren docs` ve `/docs`, plan kipi sonrası "Planı uygula? [e/H]".
- Yapıldı: prompt çerçevesi. `evren chat` ve `evren agent` `--rol/--cerceve/--brief/--kisit/--cikti`
  bayraklarını alır; bayrak ve istek yoksa uçbirimde beş satır sorulur. REPL'de Brief
  netleştirme ve boş alan önerileri (`EVREN_CLARIFY_ROUNDS`, `EVREN_FRAME_SUGGEST`,
  `EVREN_FORM_FIELDS`). `evren agent` tek seferlik görev olarak çalışır.
- Kalan: `/docs`, `/skills` ve Faz 4 komutları.
- `agent.md` yönetilen blokları, `plan_runner`, SSH araçları, skills, memory, `web_fetch`.
- TOML rol dosyaları (bu sürümde yalnız JSON).
- Kurulum dışındaki Faz 4/5 işleri: `vision_search`, MCP istemcisi, terminal kurulumu,
  güncelleyici, `/team`.
