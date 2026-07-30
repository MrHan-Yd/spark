# Spark UI 一键：杀旧进程 → 清理 → 编译 → 启动
$ErrorActionPreference = "Stop"
$Root = Split-Path $PSScriptRoot -Parent
if (-not (Test-Path "$Root\ui\Spark.UI\Spark.UI.csproj")) {
    $Root = "D:\demo\test01\spark"
}
Set-Location $Root

Write-Host "== stop old spark-ui ==" -ForegroundColor Cyan
Get-Process spark-ui -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400

Write-Host "== clean ==" -ForegroundColor Cyan
Remove-Item -Recurse -Force "ui\Spark.UI\bin","ui\Spark.UI\obj" -ErrorAction SilentlyContinue

Write-Host "== build ==" -ForegroundColor Cyan
dotnet build "ui\Spark.UI\Spark.UI.csproj" -c Debug
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$dir = Join-Path $Root "ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\win-x64"
$exe = Join-Path $dir "spark-ui.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }

Write-Host "== run ==" -ForegroundColor Cyan
Write-Host $exe
Write-Host "log: $env:LOCALAPPDATA\Spark\ui-crash.log"
Start-Process -FilePath $exe -WorkingDirectory $dir

Start-Sleep -Seconds 2
$p = Get-Process spark-ui -ErrorAction SilentlyContinue
if ($p) {
    Write-Host "OK pid=$($p.Id) hwnd=$($p.MainWindowHandle)" -ForegroundColor Green
} else {
    Write-Host "Process exited. See crash log:" -ForegroundColor Red
    $log = Join-Path $env:LOCALAPPDATA "Spark\ui-crash.log"
    if (Test-Path $log) { Get-Content $log -Tail 40 }
}
