# Build and run spark-host (from repo root)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

cargo build -p spark-host
if ($args.Count -gt 0) {
    cargo run -p spark-host -- @args
} else {
    Write-Host "Usage examples:" -ForegroundColor Cyan
    Write-Host "  .\scripts\dev_host.ps1 -- --query term"
    Write-Host "  .\scripts\dev_host.ps1 -- --toggle"
    cargo run -p spark-host -- --query ""
}
