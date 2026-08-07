# Spark UI — C# · WinUI 3

生产界面：`Spark.exe`（项目程序集名 `Spark`）。

## 结构

```text
ui/
  Spark.sln
  Spark.UI/
    Spark.UI.csproj
    App.xaml(cs)
    MainWindow.xaml(cs)      # 唯一主窗
    Program.cs
    Views/
      SearchView.*           # 搜索
      SettingsView.*         # 设置（页内切换）
    Models/CandidateDto.cs   # 对齐 Host JSON
    Services/HostIpcClient.cs
    Assets/
```

## 依赖（本机需安装）

1. [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0)
2. [Visual Studio 2022](https://visualstudio.microsoft.com/) 工作负载：**使用 C# 的 Windows 应用开发**
3. Windows App SDK（随 VS 工作负载 / 包还原）

当前仓库机器若未装 SDK，**无法在本机 `dotnet build`**；源码已齐，装好后：

```powershell
.\scripts\setup_check.ps1
.\scripts\dev_ui.ps1
```

或：

```powershell
cd ui
dotnet restore Spark.sln
dotnet build Spark.UI\Spark.UI.csproj -c Debug -p:Platform=x64
dotnet run --project Spark.UI\Spark.UI.csproj -c Debug -p:Platform=x64
```

## 行为

| 模式 | 说明 |
|------|------|
| Host 在线 | 连接命名管道 `spark.host.ipc`，`host.query` / `host.invoke` |
| Host 离线 | 使用内置演示应用列表，可单独调 UI |
| 设置 | 主窗内切换，非第二窗口 |
| 托盘 | **不在 UI 内**；由 `spark-host` 负责 |

## 与 Host 联调（Host 侧 Pipe 尚未接线时）

1. `cargo run -p spark-host -- --query term` 验证后端  
2. 单独跑 UI 看界面与演示数据  
3. 下一步：Host 增加 Named Pipe server，与 `HostIpcClient` 对齐  

协议见 `docs/DESIGN.md`、`crates/ipc`。
