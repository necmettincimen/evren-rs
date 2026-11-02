# evren-rs kurulumu (Windows, kullanıcı düzeyi; yönetici gerekmez).
#
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1 [-NoBuild]
#
# 1. `cargo build --release`
# 2. Rust ikilisini %LOCALAPPDATA%\evren\bin\evren.exe olarak kurar.
# 3. PATH'te önde duran eski Python başlatıcısını (C:\Users\<kullanıcı>\.evren-cli\bin\evren.exe,
#    sistem PATH'inde) Rust ikilisiyle değiştirir. PATH'e dokunulmaz.
# 4. Python sürümü `evren-py` adıyla erişilebilir kalır (Python Scripts\evren.exe'ye yönlenir).
# Değiştirilen Python başlatıcısı önce `<ad>.bak-<tarih>` olarak yedeklenir (önceki Rust
# kurulumu yedeklenmez, üzerine yazılır); geri almak için
# scripts\uninstall.ps1 en son yedekleri yerine koyar.

param([switch]$NoBuild)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$targets = @(
    (Join-Path $env:LOCALAPPDATA 'evren\bin\evren.exe'),
    (Join-Path $env:USERPROFILE '.evren-cli\bin\evren.exe')
)
$pyLauncher = Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\Scripts\evren.exe'

# pip/distlib başlatıcısı mı? (sonu zip EOCD kaydı PK 05 06 ile biter; Rust ikilisi bitmez)
function Test-PyLauncher([string]$path) {
    $bytes = [IO.File]::ReadAllBytes($path)
    if ($bytes.Length -lt 22) { return $false }
    $tail = $bytes[($bytes.Length - 22)..($bytes.Length - 1)]
    return ($tail[0] -eq 0x50 -and $tail[1] -eq 0x4B -and $tail[2] -eq 5 -and $tail[3] -eq 6)
}

if (-not $NoBuild) {
    foreach ($tool in 'cargo', 'dlltool') {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
            throw "$tool bulunamadı. Kurulum: winget install --id Rustlang.Rustup -e ; winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e ; rustup default stable-x86_64-pc-windows-gnu ; sonra yeni terminal açın."
        }
    }
    Write-Host '[*] cargo build --release'
    Push-Location $repo
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release başarısız ($LASTEXITCODE)" }
    } finally { Pop-Location }
}
$built = Join-Path $repo 'target\release\evren.exe'
if (-not (Test-Path $built)) { throw "Derlenmiş ikili yok: $built" }

foreach ($dest in $targets) {
    $dir = Split-Path -Parent $dest
    if (-not (Test-Path $dir)) {
        if ($dest -like "$env:LOCALAPPDATA*") {
            New-Item -ItemType Directory -Force $dir | Out-Null
        } else {
            continue
        }
    }
    if ((Test-Path $dest) -and (Test-PyLauncher $dest)) {
        $bak = "$dest.bak-$stamp"
        Copy-Item $dest $bak
        Write-Host "[*] yedek: $bak"
    } elseif (Test-Path $dest) {
        Write-Host "[*] önceki Rust kurulumu üzerine yazılıyor (yedek gerekmez): $dest"
    }
    Copy-Item $built $dest -Force
    Write-Host "[+] kuruldu: $dest"
}

# Python sürümü: evren-py.cmd, Python 3.12 Scripts başlatıcısını çağırır.
$pyCmd = Join-Path $env:LOCALAPPDATA 'evren\bin\evren-py.cmd'
if (Test-Path $pyLauncher) {
    Set-Content -Path $pyCmd -Encoding ascii -Value "@echo off`r`n`"$pyLauncher`" %*"
    Write-Host "[+] Python sürümü: $pyCmd"
} else {
    Write-Host "[!] Python başlatıcısı bulunamadı ($pyLauncher); evren-py oluşturulmadı."
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$binDir = Join-Path $env:LOCALAPPDATA 'evren\bin'
if (-not (($userPath -split ';') -contains $binDir)) {
    [Environment]::SetEnvironmentVariable('Path', "$binDir;$userPath", 'User')
    Write-Host "[+] Kullanıcı PATH'ine eklendi: $binDir (yeni terminal açın)"
}

Write-Host ''
& (Join-Path $binDir 'evren.exe') --version
Write-Host '[*] Geri almak için: powershell -ExecutionPolicy Bypass -File scripts\uninstall.ps1'
