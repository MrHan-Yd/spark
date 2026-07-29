# Spark — 技术栈定稿

> 状态：**已定稿**（2026-07-29）  
> 原则：**性能在 Rust 核心，体验在 C# WinUI 壳**  
> 相关：[ARCHITECTURE.md](./ARCHITECTURE.md) · [DESIGN.md](./DESIGN.md) · [FEATURES.md](./FEATURES.md)

---

## 1. 一句话

```text
Spark = Rust Host（热键 / 索引 / 插件 / IPC）
      + C# WinUI 3 UI（主窗 / 设置 / 列表·平铺 / 收藏）
      + 独立进程插件（任意语言 exe；WASM 为 P2）
```

**不是** Electron，**不是**以 Tauri/WebView 做主界面。

---

## 2. 定稿决策表

| 层级 | 选型 | 状态 | 说明 |
|------|------|------|------|
| 产品名 | **Spark** | 已定 | Logo：A2 四芒星 + B1 石墨黑白 |
| 平台 | **Windows 10/11 x64** | 已定 | ARM64 后期；不做 macOS/Linux 一期 |
| 宿主语言 | **Rust**（edition 2021+） | 已定 | 常驻、热路径、内存安全 |
| UI 框架 | **WinUI 3** | 已定 | 原生玻璃/Acrylic、DPI、动画 |
| UI 语言 | **C#** | **已定** | 见 §3；不用 C++/WinRT 做主 UI |
| Host↔UI | **Named Pipe + JSON-RPC** | 已定 | 接口按双进程设计 |
| MVP 进程 | **host 与 ui 可先同机双进程或紧耦合启动** | 已定方向 | 见 §4；禁止把索引/热键写进 UI 进程逻辑核心 |
| 插件默认 | **独立 exe 进程** | 已定 | 崩溃隔离、语言无关 |
| 轻插件 | **WASM（wasmtime）** | P2 | 不进 MVP |
| 索引 | **SQLite FTS5 + 内存热缓存** | 已定 | `rusqlite` |
| 热键 | `RegisterHotKey` | 已定 | **默认不用** 低层全局键盘钩子 |
| 默认热键 | **Alt+Space** | 已定 | 设置可改 **Ctrl+Space** 等 |
| IPC 编码 | 先 **JSON**，可迁 MessagePack | 已定 | ADR-5 |
| 构建 Host | **Cargo workspace** | 已定 | |
| 构建 UI | **.NET 8（或团队选定 LTS）+ WinUI 3 项目** | 已定 | `ui/` 独立 csproj |
| 安装包 | MSIX **或** Inno Setup（实现前二选一） | 待选 | 不影响开发骨架 |
| 开源协议 | 未定 | 待选 | 实现前补 `LICENSE` |

---

## 3. 为何 UI 用 C#（而不用 C++ WinUI）

| 维度 | 结论 |
|------|------|
| 性能目标 | 唤起/检索/热键在 **Rust**；UI 线程只做绑定与渲染，C# 足够 |
| 体验目标 | 设置、列表/平铺、收藏、主题：C# 生态与示例更全，迭代更快 |
| 与 Rust 集成 | **不**把 UI 链进 host 静态库强耦；用 **IPC**，语言边界清晰 |
| 风险 | 避免「全 C++」拖慢 UI 与招人；避免 Electron 牺牲内存与唤起 |

**明确拒绝作为主方案：**

- Electron / CEF 主界面  
- Tauri + WebView 作为唯一 UI（可作实验，不进主线）  
- 纯 C++/WinRT 重写主界面（无额外性能收益时不做）

---

## 4. 进程与职责（定稿）

```
┌─────────────────────────────────────────┐
│  spark-host.exe          (Rust)         │
│  · 单实例 · 自启 · 全局热键 · 托盘       │
│  · 索引 / 排序 / 配置 / 权限             │
│  · 插件进程管理 · IPC Server            │
└───────────────┬─────────────────────────┘
                │ Named Pipe（JSON-RPC）
                │ 可选：共享内存（大 payload）
┌───────────────▼─────────────────────────┐
│  spark-ui.exe            (C# WinUI 3)   │
│  · 唯一用户主窗口（搜索 / 设置页内切换） │
│  · 列表 · 平铺 · 收藏分组 · 主题        │
│  · 不负责全盘索引与插件生命周期          │
└─────────────────────────────────────────┘
                │
        ┌───────┴────────┐
        ▼                ▼
  plugin-a.exe      plugin-b.exe
  （任意语言）       （按需、可杀）
```

| 进程 | 二进制（建议名） | 语言 | 职责 |
|------|------------------|------|------|
| Host | `spark-host.exe` | Rust | 热键、托盘、索引、路由、插件、配置权威源 |
| UI | `spark-ui.exe` | C# / WinUI 3 | 主窗展示与交互；经 IPC 调 Host |
| Plugin | 插件自带 `main` | 任意 | 查询/执行；经 Host 中转 |

### 4.1 启动关系

1. 用户开机或点击 → 启动 **Host**（单实例）  
2. Host 注册热键与托盘 → **按需或预启动 UI**（预创建窗口，隐藏）  
3. 热键 → Host 通知 UI `show`（目标 P99 < 50ms）  
4. 托盘在**系统通知区域**（不是主窗角落）

### 4.2 MVP 允许的简化

- Host 启动后立刻拉起 UI 子进程并常驻隐藏（实现简单）  
- **不允许**的简化：把 SQLite 索引、插件管理写在 C# 里当长期方案  

接口与协议从第一天按 **Host ↔ UI 双进程** 写，避免以后拆不开。

---

## 5. 技术组件清单

### 5.1 Rust Host

| 用途 | 库/技术（建议） |
|------|-----------------|
| 异步运行时 | `tokio` |
| Win32 API | `windows` crate |
| 错误 | `thiserror` / `anyhow` |
| 序列化 | `serde` + `serde_json` |
| 数据库 | `rusqlite`（FTS5） |
| 日志 | `tracing` + 文件/滚动 |
| 配置 | `toml` + `%APPDATA%/Spark/` |
| WASM（P2） | `wasmtime` |

### 5.2 C# UI

| 用途 | 技术 |
|------|------|
| 框架 | WinUI 3（Windows App SDK） |
| 语言 | C#（.NET 8+ 推荐） |
| 列表 | `ListView` / `ItemsRepeater`（虚拟化） |
| 材质 | Acrylic / 系统主题；对标原型玻璃感 |
| IPC 客户端 | Named Pipe + System.Text.Json（或共享 `ipc` 契约文档） |
| MVVM | 社区常见方案即可（CommunityToolkit.Mvvm 等） |

### 5.3 插件

| 类型 | 技术 | 阶段 |
|------|------|------|
| Native | 任意语言 exe + `plugin.json` | P1 |
| SDK | 官方 **Rust SDK** 优先；C# 模板次之 | P1 |
| WASM | wasmtime 沙箱 | P2 |

---

## 6. 仓库布局（与栈对齐）

```text
spark/
  Cargo.toml                 # Rust workspace
  crates/
    host/                    # spark-host 二进制
    core/                    # 纯逻辑（可单测）
    ipc/                     # 协议类型（Rust）；文档同步给 C#
    index/
    plugin-manager/
    sdk/                     # 插件作者 Rust SDK
  ui/                        # C# WinUI 3 解决方案
    Spark.UI/
      Spark.UI.csproj
  plugins/
    echo/                    # 示例插件
  brand/                     # Logo SVG
  docs/
    TECH_STACK.md            # 本文
    ARCHITECTURE.md
    DESIGN.md
    FEATURES.md
    UI_PROTOTYPE.md
  ui-prototype/              # HTML 交互原型（非生产）
  scripts/
  .gitignore
```

**契约单一来源：**  
- 协议字段以 `docs/DESIGN.md` + `crates/ipc` 为准  
- C# 侧维护对等 DTO，或后续 codegen（非 MVP 必须）

---

## 7. 性能边界（栈如何保证）

| 目标 | 由谁保证 |
|------|----------|
| 热键 → 窗体可见 P99 < 50ms | Host 收热键 + UI **预创建隐藏窗**；不经插件 |
| 输入 → 首屏 < 30ms | Host 本地索引；UI 只渲染 |
| 常驻 < 80MB（无活跃插件） | Rust 核心精简；UI 隐藏可降资源；禁止 WebView 主路径 |
| 插件崩溃 | 独立进程；Host/UI 继续 |

**C# UI 热路径禁令：**

- 禁止 UI 线程同步扫盘、开大文件、跑插件逻辑  
- 禁止为皮肤嵌入常驻 WebView2 主搜索列表  

---

## 8. 开发环境（建议）

| 角色 | 需要 |
|------|------|
| Host | Rust stable、MSVC 工具链、Windows SDK |
| UI | Visual Studio 2022 + Windows App SDK / WinUI 3 工作负载 |
| 联调 | 先起 `spark-host`，再起 `spark-ui`；或 Host 自动 spawn UI |
| 原型 | 浏览器打开 `ui-prototype/`（仅设计参考） |

---

## 9. 已关闭的争议

| 原开放问题 | 定稿 |
|------------|------|
| UI 用 C# 还是 C++？ | **C# + WinUI 3** |
| 要不要 Electron/Tauri 主界面？ | **不要** |
| 性能是否要求 UI 也用 C++？ | **否**；性能归 Host |
| 默认热键 | **Alt+Space**，可改 Ctrl+Space |
| Logo | **A2 + B1** |

## 10. 仍待产品/工程拍板（不挡写代码骨架）

| 项 | 选项 | 备注 |
|----|------|------|
| 安装器 | MSIX / Inno | 有签名再优先 MSIX |
| .NET 具体版本 | 8 LTS / 更新 | 与 WinAppSDK 版本矩阵对齐 |
| UI 是否由 Host 强制子进程拉起 | 是（推荐） / 可手动只开 UI 调试 | 调试开关即可 |
| 开源许可证 | MIT / Apache-2.0 / 专有 | 补 LICENSE |
| 包 ID | 如 `app.spark.launcher` | 安装与插件 id 前缀 |

---

## 11. 文档索引

| 文档 | 内容 |
|------|------|
| **TECH_STACK.md（本文）** | 技术栈定稿与否决项 |
| ARCHITECTURE.md | 进程、模块、性能预算 |
| DESIGN.md | 协议、数据、模块细节、里程碑 |
| FEATURES.md | 用户功能与验收 |
| UI_PROTOTYPE.md | HTML 原型 → WinUI 映射 |

---

## 12. 修订记录

| 日期 | 变更 |
|------|------|
| 2026-07-29 | 初版定稿：Rust Host + **C# WinUI 3** + 进程插件；产品名 Spark |
