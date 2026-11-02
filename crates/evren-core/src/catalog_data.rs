// Bu dosya evren-cli src/playbooks.py ve src/templates.py verisinden üretildi.

pub(crate) const PLAYBOOK_DATA: &[(&str, &str, &str, &[&str])] = &[
    (
        r###"release"###,
        r###"Sürüm Çıkarma"###,
        r###"Test edilmiş bir sürümü güvenli biçimde yayınlar."###,
        &[
            r###"Tüm testleri çalıştır ve yeşil olduğunu doğrula."###,
            r###"CHANGELOG.md'ye sürüm notlarını ekle."###,
            r###"Sürüm numarasını pyproject.toml ve version.py'de artır."###,
            r###"Değişiklikleri commit'le ve etiketle (tag)."###,
            r###"Uzak depoya push et ve sürümü doğrula."###,
        ],
    ),
    (
        r###"hotfix"###,
        r###"Acil Hata Düzeltme"###,
        r###"Üretimdeki kritik bir hatayı en kısa yoldan düzeltir."###,
        &[
            r###"Hatayı yeniden üret ve etkisini sınırla."###,
            r###"Kök nedeni tespit et (root-cause-debug becerisi)."###,
            r###"En küçük güvenli düzeltmeyi uygula."###,
            r###"Düzeltmeyi doğrulayan bir regresyon testi ekle."###,
            r###"Düzeltmeyi yayınla ve izlemeye al."###,
        ],
    ),
    (
        r###"onboarding"###,
        r###"Projeye Katılım"###,
        r###"Yeni bir kod tabanını hızlıca anlamak için sistematik keşif."###,
        &[
            r###"Dizin yapısını ve giriş noktalarını tara (scan/list_directory)."###,
            r###"Bağımlılıkları ve çalıştırma komutlarını belirle."###,
            r###"Ana veri akışını ve mimariyi özetle."###,
            r###"Testleri çalıştır ve mevcut durumu doğrula."###,
            r###"Bulguları proje hafızasına (memory) kaydet."###,
        ],
    ),
    (
        r###"migration"###,
        r###"Kademeli Geçiş"###,
        r###"Bir kütüphane/sürüm/API geçişini kırılmasız yürütür."###,
        &[
            r###"Mevcut ve hedef arasındaki farkları ve kırıcı değişiklikleri listele."###,
            r###"Geçişi küçük, geri alınabilir adımlara böl."###,
            r###"Her adımda kodu uyarla ve testleri çalıştır."###,
            r###"Eski yolu kaldır ve kalan referansları temizle."###,
            r###"Tüm paketi doğrula ve geçişi belgele."###,
        ],
    ),
];

pub(crate) const TEMPLATE_DATA: &[(&str, &str, &str, &str, &str)] = &[
    (
        r###"incident-postmortem"###,
        r###"Olay Sonrası Analiz (Postmortem)"###,
        r###"team"###,
        r###"Bir olayın ne olduğunu, neden olduğunu ve tekrarını önleyecek düzeltmeleri belgeler."###,
        r###"# Olay Sonrası Analiz: [Başlık]

**Önem:** [ ] SEV-1 [ ] SEV-2 [ ] SEV-3 | **Yazar:** [Ad] | **Tarih:** [Tarih]

## Özet
| Alan | Detay |
|------|-------|
| Ne oldu | [2-3 cümle] |
| Başlangıç / Tespit / Çözüm | [SS:DD] / [SS:DD] / [SS:DD] |
| Etki | [Etkilenen kullanıcı, gelir, SLA] |

## Zaman Çizelgesi
| Zaman | Olay | Aktör/Sistem | Kaynak |
|-------|------|--------------|--------|
| [SS:DD] | [Tetikleyici] | [Aktör] | [Log/uyarı] |

## Kök Neden (5 Neden)
| Seviye | Soru | Cevap |
|--------|------|-------|
| Neden 1 | [Olay] neden oldu? | [Çünkü...] |
| Neden 2 | [Cevap 1] neden oldu? | [Çünkü...] |
| Neden 3 | [Cevap 2] neden oldu? | [Çünkü...] |
| Neden 4 | [Cevap 3] neden oldu? | [Çünkü...] |
| Neden 5 | [Cevap 4] neden oldu? | [Çünkü...] |

**Kök neden:** [Tek cümle — sistemik neden, tetikleyici değil]

## Katkıda Bulunan Faktörler
- [ ] Süreç boşluğu: [...]
- [ ] İzleme boşluğu: [...]
- [ ] Bilgi boşluğu: [...]
- [ ] Test boşluğu: [...]

## Uygulanan Acil Düzeltmeler
| # | Düzeltme | Uygulayan | Kalıcı mı? |
|---|----------|-----------|------------|
| 1 | [...] | [Ad] | [ ] Evet [ ] Hayır |

## Gerekli Sistemik Düzeltmeler
| # | Düzeltme | Kök neden | Sahip | Termin | Durum |
|---|----------|-----------|-------|--------|-------|
| 1 | [...] | [...] | [Ad] | [Tarih] | [ ] Başlamadı |

## Çıkarılan Dersler
1. **[Başlık]:** [...]
"###,
    ),
    (
        r###"adr"###,
        r###"Mimari Karar Kaydı (ADR)"###,
        r###"team"###,
        r###"Önemli bir teknik kararı, gerekçesi ve sonuçlarıyla belgeler."###,
        r###"# ADR-[NNN]: [Karar Başlığı]

**Durum:** [ ] Önerildi [ ] Kabul edildi [ ] Reddedildi [ ] Yerini aldı
**Tarih:** [Tarih] | **Karar verenler:** [İsimler]

## Bağlam
[Kararı gerektiren durum, kısıtlar ve problem.]

## Karar
[Alınan karar, net ve tek cümleyle.]

## Değerlendirilen Seçenekler
| Seçenek | Artılar | Eksiler |
|---------|---------|---------|
| [A] | [...] | [...] |
| [B] | [...] | [...] |

## Sonuçlar
- **Olumlu:** [...]
- **Olumsuz / ödünler:** [...]
- **Takip işleri:** [...]
"###,
    ),
    (
        r###"pr-description"###,
        r###"Pull Request Açıklaması"###,
        r###"team"###,
        r###"Bir değişikliği inceleyiciye net biçimde açıklar."###,
        r###"# [Başlık]

## Ne değişti
[Değişikliğin özeti, 2-3 madde.]

## Neden
[Problemi ve motivasyonu açıkla. İlgili issue: #NNN]

## Nasıl test edildi
- [ ] Birim testleri: `[komut]`
- [ ] Manuel doğrulama: [adımlar]

## Kontrol Listesi
- [ ] Testler eklendi/güncellendi
- [ ] Dokümantasyon güncellendi
- [ ] Kırıcı değişiklik yok (varsa belirtildi)

## Ekran Görüntüsü / Çıktı
[Varsa]
"###,
    ),
    (
        r###"release-notes"###,
        r###"Sürüm Notları"###,
        r###"user"###,
        r###"Bir sürümdeki değişiklikleri kullanıcıya duyurur."###,
        r###"# Sürüm [x.y.z] — [Tarih]

## Öne Çıkanlar
- [En önemli 1-3 değişiklik]

## Eklendi
- [...]

## Değişti
- [...]

## Düzeltildi
- [...]

## Kaldırıldı / Kırıcı Değişiklikler
- [...]

## Yükseltme Notları
[Gerekli adımlar, varsa]
"###,
    ),
    (
        r###"runbook"###,
        r###"Operasyon Kılavuzu (Runbook)"###,
        r###"ops"###,
        r###"Bir servisin çalıştırılması ve sorun giderilmesi için adım adım kılavuz."###,
        r###"# Runbook: [Servis Adı]

## Genel Bakış
[Servisin amacı, bağımlılıkları, sahipleri.]

## Sağlık Kontrolü
- Uç nokta: `[URL]`
- Beklenen yanıt: [...]

## Yaygın Sorunlar ve Çözümleri
| Belirti | Olası neden | Çözüm |
|---------|-------------|-------|
| [...] | [...] | [...] |

## Dağıtım / Geri Alma
1. [Dağıtım adımı]
2. [Geri alma adımı]

## İletişim / Eskalasyon
[Kim, hangi kanal]
"###,
    ),
    (
        r###"bug-report"###,
        r###"Hata Raporu"###,
        r###"team"###,
        r###"Bir hatayı yeniden üretilebilir biçimde raporlar."###,
        r###"# Hata: [Kısa başlık]

**Önem:** [ ] Kritik [ ] Yüksek [ ] Orta [ ] Düşük
**Ortam:** [OS, sürüm, tarayıcı/çalışma zamanı]

## Beklenen Davranış
[...]

## Gerçekleşen Davranış
[...]

## Yeniden Üretim Adımları
1. [...]
2. [...]

## Kanıt
[Log, hata mesajı, ekran görüntüsü]

## Olası Neden / Notlar
[...]
"###,
    ),
];

pub(crate) const SKILL_DATA: &[(&str, &str, &str, &str, &str, &[&str])] = &[
    (
        r###"root-cause-debug"###,
        r###"Kök Neden Hata Ayıklama"###,
        r###"debugging"###,
        r###"Belirtiyi değil, hatanın kök nedenini bulup düzeltir."###,
        r###"Bir hata/çökme/yanlış davranış bildirildiğinde."###,
        &[
            r###"Belirtiyi tek cümleyle yeniden üret ve beklenen davranışı yaz."###,
            r###"Hatayı en küçük yeniden üretim adımına indir (minimal repro)."###,
            r###"İlgili kod yolunu view_file/find_files ile oku; varsayım yapma."###,
            r###"En olası 2-3 kök nedeni sırala ve her biri için kanıt topla."###,
            r###"En olası nedeni cerrahi bir edit_file ile düzelt."###,
            r###"Düzeltmeyi doğrulayan bir test/komut çalıştır."###,
        ],
    ),
    (
        r###"surgical-refactor"###,
        r###"Cerrahi Refactor"###,
        r###"refactoring"###,
        r###"Davranışı değiştirmeden kodu küçük, güvenli adımlarla iyileştirir."###,
        r###"Kod kokusu, tekrar veya okunabilirlik sorunu olduğunda."###,
        &[
            r###"Mevcut davranışı koruyan testlerin varlığını doğrula; yoksa önce ekle."###,
            r###"Değişikliği tek bir sorumluluğa indir (tek amaçlı refactor)."###,
            r###"edit_file ile yalnızca hedef bloğu değiştir; stil ve girintiyi koru."###,
            r###"Her adımdan sonra testleri çalıştır; kırmızıysa geri al."###,
            r###"Değişikliği kısa bir özetle ve diff ile raporla."###,
        ],
    ),
    (
        r###"test-first"###,
        r###"Önce Test (TDD)"###,
        r###"testing"###,
        r###"Önce başarısız testi yazar, sonra kodu testi geçecek şekilde ekler."###,
        r###"Yeni özellik veya hata düzeltmesi eklerken."###,
        &[
            r###"Beklenen davranışı tanımlayan en küçük testi yaz."###,
            r###"Testi çalıştır ve başarısız olduğunu doğrula (kırmızı)."###,
            r###"Testi geçecek en basit kodu yaz (yeşil)."###,
            r###"Kodu tekrar eden testlerle sağlamlaştır."###,
            r###"Tüm test paketini çalıştır ve regresyon olmadığını doğrula."###,
        ],
    ),
    (
        r###"api-integration"###,
        r###"API Entegrasyonu"###,
        r###"integration"###,
        r###"Dış bir HTTP/LLM API'sini güvenli, dayanıklı biçimde entegre eder."###,
        r###"Yeni bir dış servis veya uç nokta bağlarken."###,
        &[
            r###"Uç nokta sözleşmesini (istek/yanıt şeması) netleştir."###,
            r###"Kimlik doğrulama ve hata durumlarını (4xx/5xx/timeout) ele al."###,
            r###"Retry/backoff ve idempotency stratejisini belirle."###,
            r###"İstemciyi küçük, test edilebilir bir modüle yaz."###,
            r###"Ağ çağrısını mock'layan birim testleri ekle."###,
        ],
    ),
    (
        r###"security-review"###,
        r###"Güvenlik İncelemesi"###,
        r###"security"###,
        r###"Kodda yaygın güvenlik açıklarını sistematik olarak tarar."###,
        r###"Kullanıcı girdisi, dosya I/O veya kimlik doğrulama içeren kodda."###,
        &[
            r###"Girdi noktalarını ve güven sınırlarını listele."###,
            r###"Injection (komut/SQL/path traversal) risklerini denetle."###,
            r###"Sırların (API anahtarı, parola) kodda/logda sızıp sızmadığını kontrol et."###,
            r###"Yetkilendirme ve sınır denetimlerinin eksiksiz olduğunu doğrula."###,
            r###"Bulguları önem sırasına göre ve düzeltme önerisiyle raporla."###,
        ],
    ),
    (
        r###"performance-tuning"###,
        r###"Performans İyileştirme"###,
        r###"performance"###,
        r###"Ölçüme dayalı, gereksiz optimizasyondan kaçınan performans çalışması."###,
        r###"Yavaşlık veya yüksek kaynak kullanımı bildirildiğinde."###,
        &[
            r###"Önce ölç: darboğazı profil/benchmark ile tespit et."###,
            r###"En pahalı sıcak yolu belirle; tahminle optimizasyon yapma."###,
            r###"Tek bir iyileştirme uygula ve yeniden ölç."###,
            r###"Kazanç yoksa değişikliği geri al."###,
            r###"Sonucu önce/sonra metrikleriyle raporla."###,
        ],
    ),
    (
        r###"code-review"###,
        r###"Kod İncelemesi"###,
        r###"quality"###,
        r###"Bir değişikliği doğruluk, okunabilirlik ve risk açısından inceler."###,
        r###"Bir PR/diff gözden geçirilirken."###,
        &[
            r###"Değişikliğin amacını ve kapsamını özetle."###,
            r###"Doğruluk: kenar durumları ve hata yolları ele alınmış mı?"###,
            r###"Okunabilirlik: isimlendirme, karmaşıklık, tekrar."###,
            r###"Testler: yeni davranış kapsanmış mı?"###,
            r###"Bulguları 'bloklayıcı / öneri / nit' olarak sınıflandır."###,
        ],
    ),
    (
        r###"dependency-upgrade"###,
        r###"Bağımlılık Güncelleme"###,
        r###"maintenance"###,
        r###"Bir bağımlılığı kırılma riskini yöneterek günceller."###,
        r###"Kütüphane sürümü yükseltilirken."###,
        &[
            r###"Mevcut ve hedef sürüm arasındaki değişiklik notlarını oku."###,
            r###"Kırıcı (breaking) değişiklikleri listele."###,
            r###"Sürümü güncelle ve kodu uyumlu hale getir."###,
            r###"Tüm testleri çalıştır; regresyonu doğrula."###,
            r###"Kilit dosyasını (lockfile) tutarlı biçimde güncelle."###,
        ],
    ),
];

#[allow(clippy::type_complexity)]
pub(crate) const DIAGNOSTIC_DATA: &[(
    &str,
    &str,
    &str,
    &[&str],
    &[&str],
    &[&str],
    &[&str],
    &[&str],
)] = &[
    (
        r###"build-failure"###,
        r###"Derleme/Build Hatası Triyajı"###,
        r###"Derleme veya paketleme neden başarısız oluyor sorusunu sistematik daraltır."###,
        &[
            r###"Build kırıldı"###,
            r###"Derleme hata veriyor"###,
            r###"CI kırmızı"###,
            r###"Bağımlılık çözülemiyor"###,
        ],
        &[
            r###"Hata yerel mi yoksa yalnızca CI'da mı tekrarlıyor?"###,
            r###"Son çalışan sürümden bu yana hangi dosyalar/bağımlılıklar değişti?"###,
            r###"Hata mesajı sözdizimi, tip, bağlantı (link) yoksa bağımlılık kaynaklı mı?"###,
            r###"Temiz bir ortamda (cache temizlenerek) hata tekrarlıyor mu?"###,
        ],
        &[
            r###"syntax-error"###,
            r###"type-mismatch"###,
            r###"dependency-conflict"###,
            r###"stale-cache"###,
            r###"environment-drift"###,
        ],
        &[r###"root-cause-debug"###, r###"dependency-upgrade"###],
        &[r###"hotfix"###],
    ),
    (
        r###"flaky-tests"###,
        r###"Kararsız (Flaky) Test Triyajı"###,
        r###"Bazen geçen bazen kalan testlerin kök nedenini bulur."###,
        &[
            r###"Test bazen geçiyor bazen kalıyor"###,
            r###"CI'da rastgele kırmızı"###,
            r###"Yerelde geçiyor CI'da kalmıyor"###,
        ],
        &[
            r###"Test tek başına mı yoksa tüm paketle birlikte mi kararsız?"###,
            r###"Zamanlama, sıralama veya paylaşılan durum (global state) bağımlılığı var mı?"###,
            r###"Ağ, dosya sistemi veya saat gibi dış kaynaklara bağımlı mı?"###,
            r###"Paralel çalıştırmada mı ortaya çıkıyor?"###,
        ],
        &[
            r###"race-condition"###,
            r###"test-order-dependency"###,
            r###"shared-mutable-state"###,
            r###"external-resource-dependency"###,
            r###"timing-assumption"###,
        ],
        &[r###"root-cause-debug"###, r###"test-first"###],
        &[],
    ),
    (
        r###"performance-regression"###,
        r###"Performans Gerilemesi Triyajı"###,
        r###"Yavaşlama veya kaynak artışının kaynağını ölçüme dayalı bulur."###,
        &[
            r###"Uygulama yavaşladı"###,
            r###"Yanıt süresi arttı"###,
            r###"Bellek/CPU kullanımı yükseldi"###,
        ],
        &[
            r###"Gerileme ne zaman başladı ve o tarihten sonra ne değişti?"###,
            r###"Darboğaz CPU, bellek, I/O yoksa ağ mı? (profil/ölçüm ile)"###,
            r###"Yük altında mı yoksa her koşulda mı yavaş?"###,
            r###"Değişiklik geri alındığında performans düzeliyor mu?"###,
        ],
        &[
            r###"algorithmic-complexity"###,
            r###"n-plus-one-query"###,
            r###"memory-leak"###,
            r###"blocking-io"###,
            r###"cache-miss"###,
        ],
        &[r###"performance-tuning"###, r###"root-cause-debug"###],
        &[],
    ),
    (
        r###"dependency-hell"###,
        r###"Bağımlılık Çıkmazı Triyajı"###,
        r###"Sürüm çakışmaları ve kırıcı yükseltmeleri sistematik çözer."###,
        &[
            r###"Sürüm çakışması"###,
            r###"Paket kurulamıyor"###,
            r###"Kırıcı değişiklik"###,
            r###"Peer dependency hatası"###,
        ],
        &[
            r###"Çakışan paketler ve gerektirdikleri sürüm aralıkları neler?"###,
            r###"Çakışma doğrudan mı yoksa geçişli (transitive) bir bağımlılıktan mı?"###,
            r###"Yükseltme kırıcı (breaking) değişiklik içeriyor mu?"###,
            r###"Kilit dosyası (lockfile) tutarlı mı?"###,
        ],
        &[
            r###"version-conflict"###,
            r###"transitive-conflict"###,
            r###"breaking-change"###,
            r###"lockfile-drift"###,
        ],
        &[r###"dependency-upgrade"###, r###"root-cause-debug"###],
        &[r###"migration"###],
    ),
    (
        r###"security-exposure"###,
        r###"Güvenlik Açığı Triyajı"###,
        r###"Olası bir güvenlik açığını önem ve etkiye göre daraltır."###,
        &[
            r###"Şüpheli girdi işleme"###,
            r###"Sır sızıntısı şüphesi"###,
            r###"Yetkisiz erişim riski"###,
            r###"Bağımlılık güvenlik uyarısı"###,
        ],
        &[
            r###"Girdi nereden geliyor ve doğrulanıyor mu?"###,
            r###"Sırlar (anahtar/parola) kodda, logda veya repoda görünüyor mu?"###,
            r###"Yetkilendirme ve sınır denetimleri eksiksiz mi?"###,
            r###"Etkilenen bağımlılık sürümü bilinen bir CVE içeriyor mu?"###,
        ],
        &[
            r###"injection"###,
            r###"path-traversal"###,
            r###"secret-leak"###,
            r###"missing-authorization"###,
            r###"vulnerable-dependency"###,
        ],
        &[r###"security-review"###, r###"dependency-upgrade"###],
        &[r###"hotfix"###],
    ),
    (
        r###"deploy-failure"###,
        r###"Dağıtım (Deploy) Hatası Triyajı"###,
        r###"Dağıtımın neden başarısız olduğunu veya geri alındığını daraltır."###,
        &[
            r###"Deploy başarısız"###,
            r###"Servis ayağa kalkmıyor"###,
            r###"Sağlık kontrolü (health check) geçmiyor"###,
            r###"Rollback gerekti"###,
        ],
        &[
            r###"Hata yapılandırma, bağımlılık yoksa ortam farkından mı kaynaklanıyor?"###,
            r###"Yerel/staging ortamında aynı sürüm çalışıyor mu?"###,
            r###"Ortam değişkenleri ve sırlar doğru yüklendi mi?"###,
            r###"Sağlık kontrolü ve port/bağlantı ayarları doğru mu?"###,
        ],
        &[
            r###"config-mismatch"###,
            r###"missing-env-var"###,
            r###"port-binding"###,
            r###"migration-not-applied"###,
            r###"resource-limit"###,
        ],
        &[r###"root-cause-debug"###, r###"api-integration"###],
        &[r###"hotfix"###, r###"release"###],
    ),
];
