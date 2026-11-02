# Değişiklik Günlüğü (Changelog)

Bu projedeki tüm önemli değişiklikler bu dosyada belgelenir.

Format [Keep a Changelog](https://keepachangelog.com/tr/1.1.0/) standardına,
sürümleme ise [Semantic Versioning](https://semver.org/lang/tr/) (SemVer) kurallarına dayanır.

## [4.3.1] - 2026-11-02

### Düzeltildi
- Plan `agent` adımı "değişiklik yok" yanlış alarmı: depoda izlenmeyen iç içe git deposu
  (ör. `.evren/worktrees/x/`, alt repo) varken `git hash-object --stdin-paths` `dir/` girdisinde
  düşüyor, izlenmeyen dosya özeti tümden boş kalıyordu. Önceden var olan izlenmeyen dizine
  (ör. `reports/`) yazılan rapor ya da yeni dosya bu yüzden görünmüyordu; 4.3.0'daki
  `kind: "report"` önerisi bu kök nedeni örtüyordu. `dir/` girdileri artık ad olarak özete
  giriyor, içerik hash'inden atlanıyor.

## [4.3.0] - 2026-11-02

### Eklendi
- Plan `agent` adımı `kind` alanı: `report` / `docs` / `explore` adımlarında `require_changes`
  varsayılan olarak kapalı (rapor yazan ajan "değişiklik yok" ile düşmüyor). Rapor yazılıp kod
  değişmeyen başarısızlıkta hata mesajı `kind: "report"` önerir.
- Plan `agent` adımı `inputs`: betikle hesaplanmış dosyalar (ör. `reports/ozet.txt`) prompt'a
  aynen eklenir ve ajana rakamları yeniden hesaplamaması söylenir; boş girdi adımı durdurur.
- Plan `run` adımı `save` (stdout'u dosyaya yazar) ve sayaç doğrulaması: `min_count`,
  `count_regex`, `count_warn`. Sayı yoksa ya da eşiğin altındaysa adım durur; `min_count`
  verilmemişse yalnız `0` dönen çıktıya uyarı eklenir (sessizce 0 dönen sayaçlar).
- Plan `push` adımı dal güvenliği: gönderilecek commit'ler (`origin/<target>..<branch>`) listelenir;
  yazarı yerel `user.email` olmayan commit varsa push durur ve cherry-pick komutu önerilir
  (`allow_foreign: true` ile geçilir). `target` ile `test:main` gibi refspec push'u.
- Plan `live_diff` adımı: canlı Kubernetes/Argo nesnesini (`live_cmd` ya da `live_ssh`) repodaki
  YAML ile alan yolu bazında karşılaştırır; sunucu alanları atılır, `ignore`, `live_only`,
  `fail_on_drift`, `save` desteklenir.
- Log çıktısı özeti: Loki/logcli, `kubectl logs`, journalctl, heartbeat ve zaman damgalı çıktılar
  seviye sayıları, tekrar eden kalıplar ve son hata satırlarıyla özetlenir; 40 satırı aşan log
  karakter sınırının altında da özetlenir (tam çıktı `.evren/tmp/` altında).

## [4.2.1] - 2026-11-02

### Düzeltildi
- REPL: boş Brief, form alanları kapalıyken de önceki kalıbı yeniden çalıştırır (Python gibi);
  önceki kalıp yoksa hiçbir şey yapmaz.
- `/doctor`: kaynak ağacındaki `Cargo.toml` sürümü çalışan sürümden farklıysa uyarır
  (Python `pyproject` karşılaştırması).

### Kaldırıldı
- Kullanılmayan `NOT_PORTED_YET` metni.

## [4.2.0] - 2026-11-02

### Eklendi
- MCP istemcisi (stdio, JSON-RPC 2.0): `mcpServers` ayarındaki sunucular tembel başlatılır,
  araçları normal kipte `mcp__<sunucu>__<araç>` adıyla modele sunulur; çağrı onay ister,
  REPL çıkışında sunucular kapatılır.
- `web_search`: `webSearch.url` (`{q}` yer tutuculu) ayarlıysa normal kipte sunulur.

## [4.1.2] - 2026-11-02

### Eklendi
- Araçlar sır kasasına erişir: `run_command`, arka plan komutları ve `ssh_run` komuttaki
  `{{EVREN_SECRET_n}}` yer tutucularını gerçek değerle açar; çıktı ve akış maskeli kalır.
- `describe_image` / görü modeli API anahtar havuzunu kullanır: 429'da sıradaki anahtara geçer,
  401–403'te anahtarı devre dışı bırakır.

### Düzeltildi
- `/update` ve `evren update`: ertelenen `install.ps1` çıktısı `.evren/logs/update.log`'a
  yazılır; kilitli `target\release\evren.exe` derlemeden önce `evren.exe.running-<pid>` adına
  çekilir (`scripts/install.ps1` de aynı adımı yapar); kirli çalışma ağacında uyarı verilir.
- `evren scan` uzantıları Python gibi ilk görülme sırasıyla listeler.

## [4.1.1] - 2026-11-02

### Eklendi
- `evren debug`, `evren scan`, `evren quota`, `evren rollback`, `evren setup-terminal`
  (`terminal`) ve `evren chat --file`; artık `not_ported` dönen alt komut yok.
- `search_code` (anlamsal arama, `.evren/index/index.json`) ve `describe_image` araçları;
  `view_file` görsel dosyada görü modeline gider.

### Düzeltildi
- agent adımı: önceden kirli dosyadaki düzenleme algılanmıyordu. Çalışma ağacı özeti artık
  `git diff HEAD` ve izlenmeyen dosya içeriklerinin hash'ini de içerir; gereksiz "değişiklik
  yok" yeniden denemesi ve rapora çift ekleme önlendi.
- `delegate`: boş görev, `max_steps` ayrıştırma ve alt ajan sistem istemi Python ile hizalandı.

## [4.1.0] - 2026-11-02

### Eklendi
- Python 3.11 sürümüyle komut paritesi; Faz 4 komut listesi kapandı.
- REPL: `/context`, `/compact`, `/rewind`, `/doctor`, `/rol`, `/commit`, `/review`, `/init`,
  `/files`, `/add`, `/drop`, `/scan`, `/run`, `/memory`, `/language`, `/resume`, `/keys`,
  `/debug`, `.evren/commands/*.md` özel komutları; `/docs`, `/skills`, `/skill`, `/apply`,
  `/diagnose`, `/template`, `/playbook`, `/track`, `/task`, `/progress`, `/checkpoint`,
  `/blockers`, `/check-in`, `/decisions`, `/ssh`.
- `/iterate` (`/iterasyon`) ve `evren iterate`: kontrol-düzelt döngüsü, `.evren/iterations/`
  durum dosyası, `devam`/`durum`.
- Ajan kitaplığı: yerleşik (devops, fullstack, product-owner, researcher, tester), kullanıcı ve
  proje ajanları (`.evren/agents/*.md`); `/agent`, `/agents`, `/team`, `@ajan görev`;
  `delegate` tanımlı alt ajanları çalıştırır.
- `/update` ve `evren update [--no-restart]`: checkout'ta `git pull --ff-only`, değilse
  `%LOCALAPPDATA%/evren-rs/source` klonu; Windows'ta `install.ps1` CLI kapandıktan sonra ayrı
  pencerede çalışır.
- TOML rol dosyaları (`--role x.toml`).
- `memory_save`, `web_fetch` araçları; `evren plan`; prompt çerçevesi; `agent.md`/changelog
  yönetilen blokları ve `evren docs`.

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
- Yapıldı (main): `evren plan <plan.json> [--dry-run] [--allow-push] [--from N] [-w]`
  (plan_runner, plan_agent, uzak adımlar; worktree, ajan slotları, verify kapısı).
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
- Kalan (4.1.0'da kapandı): Faz 4 komutları, TOML roller, güncelleyici, `/team`.
- Sonraya: `vision_search`, MCP istemcisi, terminal kurulumu.
