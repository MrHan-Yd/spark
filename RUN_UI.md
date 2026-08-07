# 运行 Spark UI

生产路径是 **Host + UI 双进程**。本文侧重 **UI 怎么起、怎么排错**。  
完整联调见根目录 [README.md](README.md)「启动」一节。

## 和 Host 一起用（推荐）

```powershell
# 终端 A — 后端（先清残留，避免 instance exists / 管道拒绝访问）
cd D:\demo\test01\spark
Stop-Process -Name spark-host -Force -ErrorAction SilentlyContinue
cargo run -p spark-host -- --no-ui

# 终端 B — 前端
cd D:\demo\test01\spark
.\scripts\dev_ui.ps1
```

若 Host 一启动就打印 `instance exists — forwarding toggle` 且 pipe `拒绝访问`，说明旧 `spark-host` 仍在：  
`Stop-Process -Name spark-host -Force` 后再启。详见 [README.md](README.md)「排错」。

- UI 启动后默认**隐藏**；用 **Alt+Space**（Host 热键）或 Host 托盘唤起。
- 已连接 Host 时搜索走真实开始菜单索引；未连接则用演示数据。

## 只开 UI（无 Host）

```powershell
cd D:\demo\test01\spark
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\run_ui.ps1
```

或：

```powershell
.\scripts\dev_ui.ps1
```

无 Host 时：演示列表 + UI 自己的托盘（便于单测窗口）。

## 手动三步（必须用对目录）

旧进程 / 错目录是「XAML parsing failed」最常见原因。

```powershell
# 1. 杀掉旧进程
Stop-Process -Name Spark -Force -ErrorAction SilentlyContinue

# 2. 编译
cd D:\demo\test01\spark
dotnet build ui\Spark.UI\Spark.UI.csproj -c Debug

# 3. 从 win-x64 输出目录启动（WorkingDirectory 必对）
$dir = "D:\demo\test01\spark\ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\win-x64"
Start-Process "$dir\Spark.exe" -WorkingDirectory $dir
```

> 不要用 `dotnet run` 当日常启动方式（输出路径易错）。

## 成功时表现

| 有 Host | 无 Host |
|---------|---------|
| 窗体可被 Alt+Space toggle | 需 UI 托盘或再次 Start-Process |
| 结果来自本机应用索引 | 演示数据 |
| 底栏倾向「Host · 极速」 | 演示 / 本地 |

任务栏**不**常驻按钮（ToolWindow）；图标在通知区域托盘。

## 若仍弹 XAML / 闪退

1. 任务管理器结束所有 `Spark`
2. 确认 exe 是刚编的：

```powershell
Get-Item "D:\demo\test01\spark\ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\win-x64\Spark.exe" |
  Select-Object FullName, LastWriteTime, Length
```

3. 看崩溃日志：

```powershell
Get-Content "$env:LOCALAPPDATA\Spark\ui-crash.log" -Tail 40
```

## 相关

- Host 启动与 CLI：`README.md`
- 脚本：`scripts\dev_ui.ps1`、`scripts\run_ui.ps1`、`scripts\dev_host.ps1`
