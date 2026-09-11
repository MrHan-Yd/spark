# 交付前质量门禁：Rust 全量测试 + C# 纯规则层测试。
#
# 注意：
# - cargo 需要能找到 MSVC 链接器。Git Bash 里直接跑会撞上 MSYS link.exe 遮蔽 MSVC 链接器的
#   问题（报 "link: missing operand after '\377\376'"）——用 PowerShell 跑本脚本即可。
# - C# 那部分不依赖 Windows App SDK：Spark.UI.Tests 直接 <Compile Include> 链接
#   ui/Spark.UI/Services 下的纯规则源文件（MarketRules.cs / PluginErrorCodes.cs），
#   这两个文件刻意零 WinUI 依赖——新增被测文件时在测试工程里加一条 Include。
# - 沙箱/精简 shell 可能缺 PROGRAMDATA / ProgramFiles / APPDATA，NuGet 的
#   ConfigurationDefaults 静态初始化会因 Path.Combine(null) 直接炸
#   （报 "Value cannot be null. (Parameter 'path1')"），这里补上缺省值兜底。
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not $env:PROGRAMDATA)  { $env:PROGRAMDATA  = "$env:SystemDrive\ProgramData" }
if (-not $env:ProgramFiles) { $env:ProgramFiles = "$env:SystemDrive\Program Files" }
if (-not $env:APPDATA)      { $env:APPDATA      = "$env:USERPROFILE\AppData\Roaming" }
if (-not $env:LOCALAPPDATA) { $env:LOCALAPPDATA = "$env:USERPROFILE\AppData\Local" }

Write-Host "== cargo test --workspace =="
cargo test --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "== dotnet test (Spark.UI.Tests: 市场规则/错误码) =="
dotnet test ui/Spark.UI.Tests/Spark.UI.Tests.csproj --nologo
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "ALL TESTS PASSED"
