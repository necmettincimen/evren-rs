KUBERNETES NOTLARI:
- Kullanıcı bir host/alias (ör. koyu3-k8s) verdiyse kubectl'i O host'ta ssh_run ile çalıştır; yerel run_command
  ile kubectl çalıştırma (yerel kubeconfig BAŞKA bir kümeyi gösterebilir). Ön kontrol (`kubectl version`,
  `get nodes`) yapma; `-w/--watch` ve uzun `sleep` kullanma (zaman aşımı).
- Hazır salt-okuma komutları (komut yazma, preset ver): ssh_run(alias, preset="k8s_health") = pod/küme durumu,
  öneri, teşhis için tek anlık görüntü; ssh_run(alias, preset="k8s_resources") = container başına request/limit
  tablosu. Tek namespace için namespace="<ns>".
- Restart sayısını Running podlarda da değerlendir (Running ≠ sağlıklı). Birçok podun son restart'ı aynı
  zamandaysa boot / k3s başlangıç zamanıyla karşılaştır. Ayrıntı gerekirse yalnızca şüpheli pod için
  `kubectl -n <ns> describe pod <p> | tail -25` veya `kubectl -n <ns> logs <p> --previous --tail=40`.
- Uzun liste çıktısını EVREN kullanıcının terminaline tam basar; tabloyu yeniden yazma, kısa özet ver.
