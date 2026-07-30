# Spark

Windows 效率启动器：**性能优先**的全局唤起工具（类似 uTools / PowerToys Run）。

| | |
|--|--|
| Host | **Rust** · `spark-host`（热键 / 索引 / 启动 / 托盘 / IPC） |
| UI | **C# WinUI 3** · `spark-ui`（搜索窗） |
| 插件 | 独立进程 + `plugin.json`（P1） |
| 热键 | 默认 **Alt+Space**（可改，见配置） |
| IPC | Named Pipe `\\.\pipe\spark.host.ipc` |

## 仓库结构

```text
spark/
├── crates/
│   ├── host/             # spark-host 二进制
│   ├── core/             # 领域模型 / 排序
│   ├── ipc/              # JSON-RPC 协议类型
│   ├── index/            # 应用索引 + 历史
│   ├── plugin-manager/   # 清单扫描
│   └── sdk/              # 插件 Rust SDK
├── plugins/echo/         # 示例插件
├── ui/Spark.UI/          # C# WinUI → spark-ui.exe
├── brand/                # Logo
├── docs/                 # 架构 / 设计 / 功能 / 技术栈
├── ui-prototype/         # HTML 交互原型（非生产）
└── scripts/
```

## 文档

| 文档 | 说明 |
|------|------|
| [docs/TECH_STACK.md](docs/TECH_STACK.md) | **技术栈定稿** |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 架构 |
| [docs/DESIGN.md](docs/DESIGN.md) | 协议与详细设计 |
| [docs/FEATURES.md](docs/FEATURES.md) | 功能 |
| [RUN_UI.md](RUN_UI.md) | UI 单独启动 / 排错 |
| [docs/UI_PROTOTYPE.md](docs/UI_PROTOTYPE.md) | 原型说明 |

## 环境

| 组件 | 要求 |
|------|------|
| Host | Rust stable（`rust-toolchain.toml`）+ MSVC 工具链 |
| UI | **.NET 8 SDK** + VS2022「Windows 应用开发」/ Windows App SDK |
| 检查 | `.\scripts\setup_check.ps1` |

---

## 启动（推荐：双进程）

正式路径是 **先 Host、再 UI**。Host 管热键与索引；UI 只负责窗口。

在仓库根目录 `spark/` 打开 **两个** PowerShell：

### 1. 后端 Host

```powershell
cd D:\demo\test01\spark

# 若曾后台跑过 Host，先清残留（否则会 instance exists / 管道拒绝访问）
Stop-Process -Name spark-host -Force -ErrorAction SilentlyContinue

# 编译（可选）
cargo build -p spark-host

# 常驻：热键 + 托盘 + Named Pipe（不要关这个窗口，或改用后台启动）
cargo run -p spark-host -- --no-ui
```

| 参数 | 含义 |
|------|------|
| （无） | 常驻；若找到 `spark-ui.exe` 会尝试自动拉起 UI |
| `--no-ui` | 只起 Host，**不**自动拉 UI（联调时推荐） |
| `--query term` | 一次性搜索，打印 JSON 后退出 |
| `--query 记事 --launch` | 搜索并启动第一条 |
| `--toggle` | 通知**已在运行**的 Host 切换 UI 显隐 |
| `--allow-second` | 开发用，跳过单实例 |

**后台启动示例：**

```powershell
Start-Process -FilePath ".\target\debug\spark-host.exe" `
  -ArgumentList "--no-ui" `
  -WorkingDirectory (Get-Location) `
  -WindowStyle Hidden
```

成功时：

- 系统托盘（任务栏旁 ▲）出现 Spark
- 默认热键 **Alt+Space** 已注册
- 管道 `spark.host.ipc` 在听

配置 / 历史目录：`%APPDATA%\Spark\`（`config.toml`、`history.json`）。

### 2. 前端 UI

```powershell
cd D:\demo\test01\spark

# 一键编译并启动（推荐）
.\scripts\dev_ui.ps1

# 或完整清理后启动
.\scripts\run_ui.ps1
```

**手动：**

```powershell
Stop-Process -Name spark-ui -Force -ErrorAction SilentlyContinue
dotnet build ui\Spark.UI\Spark.UI.csproj -c Debug

$dir = "D:\demo\test01\spark\ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\win-x64"
Start-Process "$dir\spark-ui.exe" -WorkingDirectory $dir
```

> 必须从 **`win-x64` 输出目录** 启动，并设置 `WorkingDirectory`。  
> 不要用 `dotnet run`（容易跑错目录）。详见 [RUN_UI.md](RUN_UI.md)。

成功时：

- 进程 `spark-ui` 在跑；窗口默认**隐藏**（uTools 风格）
- 已连上 Host 时，底栏/状态为 **Host · 极速**（未连则为演示数据）
- **Alt+Space** 或 Host 托盘「显示」→ 弹出搜索窗
- 输入搜开始菜单应用 → **Enter** 由 Host 启动

### 3. 联调验收清单

1. Host 日志出现 `IPC server listening`、`hotkey registered`
2. UI 起来后 Host 日志出现 `UI connected`
3. `Alt+Space` 显示 / 再按隐藏（toggle）
4. 搜「记事」等能出真实应用；Enter 能打开
5. 再开一次 `spark-host` 应转发 toggle，而不是双开 Host

---

## 仅 UI / 仅 Host（调试）

| 场景 | 做法 |
|------|------|
| 只看界面、无 Host | 只跑 `.\scripts\dev_ui.ps1` → 使用**演示数据**，本地托盘可显示窗 |
| 只测索引/启动 | `cargo run -p spark-host -- --query term` / `--launch` |
| 只测管道 toggle | Host 常驻后：`cargo run -p spark-host -- --toggle` |

---

## 构建与测试

```powershell
cd D:\demo\test01\spark
cargo build --workspace
cargo test --workspace
cargo fmt   # 有 Rust 改动时

dotnet build ui\Spark.UI\Spark.UI.csproj -c Debug
```

示例插件：

```powershell
cargo run -p spark-plugin-echo -- hello
```

---

## 停止

```powershell
Stop-Process -Name spark-host, spark-ui -Force -ErrorAction SilentlyContinue
```

也可：Host 托盘 → **退出**（结束 Host；UI 需自行关或再 Stop-Process）。

---

## 排错

| 现象 | 处理 |
|------|------|
| 热键无反应 | 是否已起 `spark-host`？是否被其它软件占用 Alt+Space？看 `%APPDATA%\Spark\config.toml` |
| UI 一直演示数据 | Host 未开或未连上 pipe；先 Host 再 UI，看 Host 是否 `UI connected` |
| 搜不到本机应用 | Host 需成功枚举开始菜单；看启动日志 `enumerated start-menu apps count=…` |
| UI 闪退 / XAML | 见 [RUN_UI.md](RUN_UI.md)；日志 `%LOCALAPPDATA%\Spark\ui-crash.log` |
| **`instance exists — forwarding toggle`** 后 **`拒绝访问` / CreateFile pipe 失败** | 已有**残留** `spark-host`（常来自先前后台联调），mutex 被占但管道不可用。先杀再启，见下 |

### 残留 Host / 单实例冲突（常见）

日志类似：

```text
instance exists — forwarding toggle
forward toggle failed … CreateFile pipe … 拒绝访问 (0x80070005)
```

含义：本机已有一个 `spark-host` 占着单实例锁；新进程不会真正启动，只会尝试经 Named Pipe 通知旧实例。若旧实例已僵死或管道权限异常，就会报「拒绝访问」。

**处理（启动前建议养成习惯）：**

```powershell
# 查看
Get-Process spark-host -ErrorAction SilentlyContinue

# 结束所有 Host（必要时连 UI 一起清）
Stop-Process -Name spark-host, spark-ui -Force -ErrorAction SilentlyContinue

# 再启动
cd D:\demo\test01\spark
cargo run -p spark-host -- --no-ui
```

确认成功日志应包含：`IPC server listening`、`hotkey registered`、`tray ready`（而不是立刻 `instance exists` 后退出）。

---

## 原型预览（非生产）

```powershell
Start-Process ui-prototype\index.html
Start-Process brand\logo-preview.html
```

## 许可

待定（见 TECH_STACK §10）。
