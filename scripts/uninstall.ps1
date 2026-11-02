# evren-rs kurulumunu geri alır: install.ps1'in değiştirdiği her evren.exe için en son
# `evren.exe.bak-<tarih>` yedeğini yerine koyar ve evren-py.cmd'yi siler.
#
#   powershell -ExecutionPolicy Bypass -File scripts\uninstall.ps1

$ErrorActionPreference = 'Stop'
$targets = @(
    (Join-Path $env:LOCALAPPDATA 'evren\bin\evren.exe'),
    (Join-Path $env:USERPROFILE '.evren-cli\bin\evren.exe')
)

foreach ($dest in $targets) {
    $dir = Split-Path -Parent $dest
    if (-not (Test-Path $dir)) { continue }
    $bak = Get-ChildItem -Path $dir -Filter 'evren.exe.bak-*' |
        Sort-Object Name -Descending | Select-Object -First 1
    if ($null -eq $bak) {
        Write-Host "[!] yedek yok, dokunulmadı: $dest"
        continue
    }
    Copy-Item $bak.FullName $dest -Force
    Write-Host "[+] geri yüklendi: $dest <- $($bak.Name)"
}

$pyCmd = Join-Path $env:LOCALAPPDATA 'evren\bin\evren-py.cmd'
if (Test-Path $pyCmd) {
    Remove-Item $pyCmd -Force
    Write-Host "[+] silindi: $pyCmd"
}

Write-Host ''
evren --version
