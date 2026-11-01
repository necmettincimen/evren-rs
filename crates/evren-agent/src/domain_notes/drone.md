DRONE CI API NOTLARI:
- Kimlik doğrulama: `GET /api/user` (200 = token geçerli).
- Her reponun SON build'i tek istekte gelir: `GET /api/user/repos?latest=true`; her repo nesnesinde
  `build` alanı son build'dir (`number`, `status`: success|failure|error|killed|running|pending,
  `target`, `event`, `finished`). Son build'i hatalı repolar için repo başına istek atma; bu
  yanıtı süz (status failure|error|killed). `build` alanı olmayan repo hiç build almamıştır.
  `latest=true` olmadan `/api/user/repos` build bilgisi döndürmez.
- Ayrıntı yalnızca istenirse: `GET /api/repos/{owner}/{repo}/builds/{number}` (stages[].number,
  stages[].steps[]: number, name, status, exit_code). Log: `GET .../builds/{number}/logs/{stage_number}/{step_number}`
  — adım ADI değil NUMARASI (isim verilirse 400 strconv.Atoi); yanıt `[{pos,out,time}]` dizisidir, `out` alanlarını birleştir.
