HTTP VE ARGO CD NOTLARI:
- run_command çıktısı 8000 karakterde kırpılır: büyük JSON yanıtını ham yazdırma. PowerShell
  (use_powershell=true, Invoke-RestMethod) veya python ile yalnızca gereken alanları süzüp yazdır.
- curl gövdesini JSON sanma. İstek `-sS -w "\nHTTP:%{http_code}"` ile gitsin. Kod 200 ve gövde JSON değilse parse etme; kodu ve ilk baytları yaz, aynı aileyi uydurma.
- Argo CD uygulama özeti: `GET /api/v1/applications/{ad}` içindeki `status.resources` (kind, name, sync, health, message).
- Pod ve canlı ağaç: `GET /api/v1/applications/{ad}/resource-tree`. Pod düğümleri `nodes` içindedir (`kind==Pod`).
- Tek nesne: `GET /api/v1/applications/{ad}/resource?namespace=&resourceName=&name=&version=&kind=&group=`.
- Pod logu: `GET /api/v1/applications/{ad}/logs?namespace=&podName=&container=&tailLines=100`. Bu uç metin döner, JSON değildir.
- Şu yollar yoktur; 404 alınca tekrarlama: `/pods`, `/resource/pods`, `/resource/tree`.
- Çoklu uç denemesi (1..N aralığı, birden çok aday yol) TEK run_command çağrısında tek satırlık döngüyle
  yapılır; her uç için ayrı çağrı açma: `for i in $(seq 1 50); do code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 8 "$BASE/api/builds/1/logs/$i"); echo "$i $code"; [ "$code" = 200 ] && break; done`
  Döngü 200'de `break` eder; 404/403/5xx'te aynı ucu tekrar denemez, sıradaki uca geçer. Tüm uçlar aynı
  taban adreste 404 ise taban yanlıştır: döngüyü tekrarlama, tek satırda "çalışan uç yok" yaz.
  İlk 3 uç da 404 ise taramayı sürdürme (50 istek ~60 sn sürer): döngüyü `[ "$code" = 404 ] && [ "$i" -ge 3 ] && break`
  ile erken bitir, çıktıyı `| tail -3` ile kısalt.
- `sync: OutOfSync` ve `health: Missing` kaynak henüz uygulanmamıştır; pod arama. `exceeded its progress deadline` yeni ReplicaSet'in hazır olmamasıdır; resource-tree içindeki Pod ve ardından logs ucuna bak.
