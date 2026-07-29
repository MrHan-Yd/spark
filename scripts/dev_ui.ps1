# Build and launch spark-ui
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
    Write-Host "未检测到 .NET SDK" -ForegroundColor Red
    exit 1
}

dotnet build "$root\ui\Spark.UI\Spark.UI.csproj" -c Debug -p:Platform=x64
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# 可能路径（按优先级）
$candidates = @(
    "$root\ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\win-x64\spark-ui.exe",
    "$root\ui\Spark.UI\bin\x64\Debug\net8.0-windows10.0.19041.0\win-x64\spark-ui.exe",
    "$root\ui\Spark.UI\bin\x64\Debug\net8.0-windows10.0.19041.0\spark-ui.exe",
    "$root\ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\spark-ui.exe"
)

$exe = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $exe) {
    Write-Host "未找到 spark-ui.exe，搜索 bin 目录..." -ForegroundColor Yellow
    $exe = Get-ChildItem -Path "$root\ui\Spark.UI\bin" -Recurse -Filter "spark-ui.exe" -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty FullName
}

if (-not $exe) {
    Write-Host "构建成功但找不到 spark-ui.exe" -ForegroundColor Red
    exit 1
}

Write-Host "启动: $exe" -ForegroundColor Cyan
Start-Process $exe
