$ErrorActionPreference = "Continue"
Write-Host "=== Spark 环境检查 ===" -ForegroundColor Cyan

Write-Host "`n[Rust]"
rustc --version 2>$null
cargo --version 2>$null
if (-not $?) { Write-Host "  缺少 Rust" -ForegroundColor Red } else { Write-Host "  OK" -ForegroundColor Green }

Write-Host "`n[.NET]"
dotnet --version 2>$null
if (-not $?) {
    Write-Host "  缺少 .NET SDK（UI 需要 net8）" -ForegroundColor Yellow
    Write-Host "  https://dotnet.microsoft.com/download/dotnet/8.0"
} else {
    Write-Host "  OK" -ForegroundColor Green
    dotnet --list-sdks
}

Write-Host "`n[Workspace]"
Set-Location (Split-Path $PSScriptRoot -Parent)
if (Test-Path Cargo.toml) { Write-Host "  Cargo.toml OK" -ForegroundColor Green }
if (Test-Path ui\Spark.UI\Spark.UI.csproj) { Write-Host "  Spark.UI.csproj OK" -ForegroundColor Green }
if (Test-Path ui-prototype\index.html) { Write-Host "  ui-prototype OK" -ForegroundColor Green }

Write-Host "`n完成。"
