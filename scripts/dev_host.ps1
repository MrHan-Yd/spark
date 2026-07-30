# Build and run spark-host (from repo root)
#   .\scripts\dev_host.ps1              → 常驻 Host（--no-ui，不自动拉 UI）
#   .\scripts\dev_host.ps1 -- --query term
#   .\scripts\dev_host.ps1 -- --toggle
#   .\scripts\dev_host.ps1 -Background  → 后台 Hidden 启动
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

cargo build -p spark-host
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$background = $false
$killFirst = $false
$forward = @()
foreach ($a in $args) {
    if ($a -eq "-Background" -or $a -eq "--background") { $background = $true }
    elseif ($a -eq "-Kill" -or $a -eq "--kill" -or $a -eq "-Force") { $killFirst = $true }
    else { $forward += $a }
}

$exe = Join-Path (Get-Location) "target\debug\spark-host.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }

if ($forward.Count -eq 0) {
    $forward = @("--no-ui")
}

# 常驻启动前默认清残留，避免 instance exists + pipe 拒绝访问
# 一次性子命令（--query / --toggle / --launch）不杀，以免误伤正在用的 Host
$isOneShot = $forward | Where-Object { $_ -eq "--query" -or $_ -eq "--toggle" -or $_ -eq "--launch" }
if ($killFirst -or -not $isOneShot) {
    $old = Get-Process spark-host -ErrorAction SilentlyContinue
    if ($old) {
        Write-Host "Stopping existing spark-host (pid $($old.Id -join ', '))…" -ForegroundColor Yellow
        $old | Stop-Process -Force
        Start-Sleep -Milliseconds 400
    }
}

if ($background) {
    Write-Host "Background: $exe $($forward -join ' ')" -ForegroundColor Cyan
    Start-Process -FilePath $exe -ArgumentList $forward -WorkingDirectory (Get-Location) -WindowStyle Hidden
    Write-Host "Started. Tray + Alt+Space. Stop: Stop-Process -Name spark-host" -ForegroundColor Green
} else {
    Write-Host "Running: spark-host $($forward -join ' ')" -ForegroundColor Cyan
    Write-Host "Ctrl+C to stop (if message loop). UI: .\scripts\dev_ui.ps1" -ForegroundColor DarkGray
    & $exe @forward
}
