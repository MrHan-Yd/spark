# Spark

Windows 效率启动器：**性能优先**的全局唤起工具（类似 uTools / PowerToys Run）。

| | |
|--|--|
| Host | **Rust** · `spark-host` |
| UI | **C# WinUI 3** · `spark-ui` |
| 插件 | 独立进程 + `plugin.json` |
| 热键 | 默认 `Alt+Space`（可改 `Ctrl+Space`） |

## 仓库结构

```text
spark/
├── crates/
│   ├── host/             # spark-host 二进制
│   ├── core/             # 领域模型 / 排序
│   ├── ipc/              # JSON-RPC 协议类型
│   ├── index/            # 搜索索引（MVP 内存）
│   ├── plugin-manager/   # 清单扫描与生命周期
│   └── sdk/              # 插件 Rust SDK
├── plugins/echo/         # 示例插件
├── ui/                   # C# WinUI（VS 创建正式工程）
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
| [docs/UI_PROTOTYPE.md](docs/UI_PROTOTYPE.md) | 原型说明 |

## 环境

| 组件 | 要求 |
|------|------|
| Host | Rust stable（`rust-toolchain.toml`）+ MSVC |
| UI | **.NET 8 SDK** + VS2022「Windows 应用开发」+ Windows App SDK |
| 检查 | `.\scripts\setup_check.ps1` |

## 快速开始

### Host（Rust，已可运行）

```powershell
cd spark
cargo build --workspace
cargo test --workspace
cargo run -p spark-host -- --query term
cargo run -p spark-plugin-echo -- hello
```

### UI（C# WinUI，需本机 .NET 8）

```powershell
# 推荐
.\scripts\dev_ui.ps1

# 或：
dotnet build ui\Spark.UI\Spark.UI.csproj -c Debug -p:Platform=x64
# 产物通常在：
Start-Process ui\Spark.UI\bin\Debug\net8.0-windows10.0.19041.0\win-x64\spark-ui.exe
```

> 尽量用 **`.\scripts\dev_ui.ps1`** 或 **直接 Start-Process exe**。  
> `dotnet run -p:Platform=x64` 容易找错输出目录，不推荐。

Host 未开时 UI 使用**演示数据**；已启用 **WindowsAppSDK SelfContained**（自带运行时）。

当前 Host 为控制台 MVP（内存索引 + 插件扫描）。热键 / 托盘 / Pipe server 按里程碑推进。

## 原型预览

```powershell
Start-Process ui-prototype\index.html
Start-Process brand\logo-preview.html
```

## 许可

待定（见 TECH_STACK §10）。
