# Build the Spark installer and latest.json release manifest (run from repo root)
#   .\scripts\build_installer.ps1 -Version 0.2.0                  # default: assemble from Release outputs
#   .\scripts\build_installer.ps1 -Version 0.2.0 -Tag v0.2.0 -UiDir "dist-publish"
# Requires: Inno Setup 6 (ISCC.exe, e.g. choco install innosetup -y)
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Tag = "v$Version",
    [string]$HostExe = "target\release\spark-host.exe",
    [string]$UiDir = "ui\Spark.UI\bin\Release\net8.0-windows10.0.19041.0\win-x64",
    [string]$RepoUrl = "https://github.com/MrHan-Yd/spark"
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

# ---- 1. locate ISCC ----
$iscc = $null
foreach ($p in @(
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Inno Setup 6\ISCC.exe")) {
    if (Test-Path $p) { $iscc = $p; break }
}
if (-not $iscc) {
    $cmd = Get-Command ISCC -ErrorAction SilentlyContinue
    if ($cmd) { $iscc = $cmd.Source }
}
if (-not $iscc) { throw "Inno Setup 6 (ISCC.exe) not found. Install it first: choco install innosetup -y" }

# ---- 2. verify build outputs and stage installer\dist\ (host exe + full UI output) ----
$hostExePath = Join-Path (Get-Location) $HostExe
$uiDirPath = Join-Path (Get-Location) $UiDir
foreach ($p in @($hostExePath, $uiDirPath)) {
    if (-not (Test-Path $p)) { throw "Missing build output: $p" }
}

$dist = Join-Path (Get-Location) "installer\dist"
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Path $dist | Out-Null
Copy-Item $hostExePath $dist
Copy-Item (Join-Path $uiDirPath "*") $dist -Recurse -Force
Write-Host "Staged dist: $dist"

# ---- 3. compile the installer ----
& $iscc "installer\spark.iss" "/DAppVersion=$Version" "/DSourceDir=$dist"
if ($LASTEXITCODE -ne 0) { throw "ISCC compile failed (exit $LASTEXITCODE)" }

# ---- 4. write latest.json (the in-app check/download manifest) ----
$setup = Join-Path (Get-Location) "installer\output\Spark-$Version-setup.exe"
if (-not (Test-Path $setup)) { throw "Missing installer: $setup" }

$sha = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLowerInvariant()
$manifest = @{
    version = $Version
    url     = "$RepoUrl/releases/download/$Tag/Spark-$Version-setup.exe"
    sha256  = $sha
}
$manifestPath = Join-Path (Get-Location) "installer\output\latest.json"
# UTF-8 without BOM so JSON parsers don't choke
[System.IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Compress), (New-Object System.Text.UTF8Encoding $false))

Write-Host ""
Write-Host "Installer: $setup" -ForegroundColor Green
Write-Host "Manifest:  $manifestPath" -ForegroundColor Green
Write-Host "sha256:    $sha" -ForegroundColor Green
