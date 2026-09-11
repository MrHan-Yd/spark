# MEMORY.md — Spark 项目长期约定

## 技术栈与硬性约束（详见 `AGENTS.md` + `docs/TECH_STACK.md`）
- Rust Host（`crates/`）+ C# WinUI 3 UI（`ui/Spark.UI/`）。热路径逻辑（索引/热键/搜索）
  只进 `crates/`，**绝不**做进 UI 层。
- `ui-prototype/` 只是设计参考，不是生产 UI 代码。
- 每次交付前：`cargo test --workspace` 必须绿；含 Rust 改动必须 `cargo fmt`。
- 改动后必须走 Code Auditor 审计闭环（只找 bug/架构违规、不代写代码），
  `NEEDS_FIX` 则主模型自修并回归，直至 PASSED 才交付。

## 构建与验证命令
- **Rust**：Git Bash 里必须先导出 MSVC 环境，否则链接失败（见 `~/.workbuddy/MEMORY.md`）。
- **UI**：`dotnet build ui/Spark.UI/Spark.UI.csproj -c Debug -p:Platform=x64 --no-restore`。
  判定看有没有 `error CS` / `error XLS`；`MSB3021/3026/3027` 只是产物被运行中的程序占用。
- **C# 纯规则层测试**：`dotnet test ui/Spark.UI.Tests/Spark.UI.Tests.csproj`（59 用例）。
  工程不引用 WinUI，靠 `<Compile Include>` 链接 `Services/MarketRules.cs`、`PluginErrorCodes.cs`
  ——这两个文件**必须保持零 WinUI 依赖**（护栏）。一条命令跑完 Rust+C#：`.\scripts\test.ps1`。
- **e2e**：`scripts/e2e/*.ps1` 直连命名管道 `\\.\pipe\spark.host.ipc`，免 UI。跑前需
  `taskkill /IM spark-host.exe /F`（安装版 watchdog 会拉安装版 host 抢单实例），
  且先 `cargo build -p spark-host`（`cargo test` 不重链 bin）。全部 ASCII only。
- 剪贴板相关 e2e 必须在交互桌面跑（无头会话连 .NET 的剪贴板都打不开）。

## 代码约定
- host 锁序纪律：阻塞 IO（HTTP / 文件 / 进程 / 剪贴板 / 图片编解码 / native RPC）一律
  **锁内只做鉴权 + 纯参数解析**，执行放 `ipc_server.rs` 锁外。
- 错误码约定：host 用 `"CODE: detail"` 形式回错误，UI `PluginWindow.ClassifyError` 还原
  `error.code`。**新增错误码必须同步改 `ClassifyError` 的识别列表**，否则静默降级成 UNAVAILABLE。
- 清单 `plugin.json` 字段名一律 snake_case；`window` 的 camelCase 写法在
  `PluginManifest::load` 里由 `normalize_window_keys` 归一化（正名优先）。
- 三方不可信数据（registry.json / 市场索引）在入口统一净化（如 `RegistryService.NormalizeTags`）。
- `crates/plugin-manager/src/signing/` 的 canonicalization 被 plugin-manager 与 sign-tool 共用，
  改一处即两端生效；`TRUSTED_KEYS` 里当前是**开发密钥**，发布前必须离线机重生成。

## 文档同步点（改功能时别漏）
- `插件开发/插件开发规范.md`：§4.3 window 字段、§5 触发方式、§6 窗口生命周期、§7 权限、
  §8 spark.* API 与语义、§8.6 错误码表、§15 IPC 表。
- `插件开发/插件市场与仓库.md`：§3.2/3.3 索引 schema、§9.5 容错表。
- `插件开发/WebView插件开发.md` / `Native插件开发.md`：清单字段与 API 状态表。
- `docs/FEATURES.md`：功能总览与插件用户视角章节。
- `docs/PLUGIN_MARKETPLACE_TASKS.md` / `docs/PLUGIN_SIGNING_TASKS.md` / `docs/插件签名安全整改清单.md`：
  勾选进度 + 记录实测证据（这些清单被当作交付凭证看）。
