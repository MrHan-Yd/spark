# Spark — 架构文档

> 产品：**Spark** — Windows 专用、性能优先的全局效率启动器  
> 宿主：**Rust**（`spark-host`）  
> UI：**C# + WinUI 3**（`Spark`）— 定稿见 [TECH_STACK.md](./TECH_STACK.md)  
> 插件：开放生态（独立进程；WASM 为 P2）

---

## 1. 目标与非目标

### 1.1 目标

| 维度 | 目标 |
|------|------|
| 性能 | 热键唤起 P99 < 50ms；输入到首屏结果 < 30ms；常驻工作集 < 80MB |
| 体验 | 全局热键、托盘、极速模糊搜索、可扩展命令面板 |
| 插件 | 官方与第三方可独立开发、安装、更新；崩溃不拖垮宿主 |
| 平台 | 仅 Windows 10/11 x64（后续可扩 ARM64） |

### 1.2 非目标（一期不做）

- 跨平台（macOS/Linux）
- 完整 IDE / 浏览器内嵌生态
- 默认所有插件使用 WebView
- 云同步账号体系（可二期）

---

## 2. 总体架构

```
┌─────────────────────────────────────────────────────────────────┐
│                        Windows Shell / 用户                       │
│              全局热键 / 托盘点击 / 协议唤起 / 命令行                 │
└───────────────────────────────┬─────────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────────┐
│                     Host Process (Rust)                          │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────┐ │
│  │ Hotkey   │ │ Tray     │ │ Router   │ │ Indexer  │ │ Plugin │ │
│  │ Manager  │ │ Manager  │ │ / Ranker │ │ Engine   │ │ Mgr    │ │
│  └──────────┘ └──────────┘ └────┬─────┘ └────┬─────┘ └───┬────┘ │
│                                 │            │            │      │
│  ┌──────────────────────────────▼────────────▼────────────▼────┐ │
│  │              Core Services (async runtime: tokio)            │ │
│  │   IPC Server · Config · Permission · Update · Telemetry      │ │
│  └──────────────────────────────┬───────────────────────────────┘ │
└─────────────────────────────────┼─────────────────────────────────┘
                                  │ Named Pipe / Shared Memory
          ┌───────────────────────┼───────────────────────┐
          │                       │                       │
┌─────────▼─────────┐   ┌─────────▼─────────┐   ┌─────────▼─────────┐
│  UI Process       │   │  Plugin Process   │   │  Plugin Process   │
│  (WinUI 3 /      │   │  (任意语言 exe)    │   │  (任意语言 exe)    │
│   原生壳)         │   │  或 WASM 沙箱     │   │                   │
└───────────────────┘   └───────────────────┘   └───────────────────┘
```

### 2.1 设计原则

1. **热路径极简**：热键 → 显示窗口 → 本地检索，不经过插件进程（除非显式命令）
2. **崩溃隔离**：第三方插件默认独立进程；宿主永不因插件 panic 退出
3. **按需唤醒**：UI 与插件延迟加载；隐藏时释放非关键资源
4. **契约稳定**：插件只依赖 IPC 协议与 SDK，不依赖宿主内部结构
5. **权限最小化**：插件声明权限，用户授权后才可访问剪贴板/文件系统等

---

## 3. 进程模型

| 进程 | 职责 | 生命周期 | 语言 |
|------|------|----------|------|
| **host**（`spark-host`） | 热键、托盘、索引、路由、插件管理、IPC | 常驻 | **Rust** |
| **ui**（`Spark`） | 搜索框、结果列表、设置页、主题、收藏 | 常驻隐藏或随 Host 拉起 | **C# + WinUI 3** |
| **plugin-*** | 具体能力（翻译、OCR、截图等） | 按需启动，空闲回收 | 任意（Rust/C#/Go/Python…） |
| **indexer-worker**（可选） | 重扫描、缩略图、深目录遍历 | 任务型 | Rust |

### 3.1 为何 UI 独立进程（C#）

- UI 崩溃不丢索引与热键  
- **性能热路径留在 Rust**；C# 只负责呈现与交互  
- 语言边界清晰：Host↔UI 仅 IPC，避免 C++/C#/Rust 混链复杂度  

**落地节奏（定稿）：**

- **MVP**：Host + UI **双进程**（Host 可自动 spawn UI）+ 插件独立进程；协议按双进程写死  
- **不做**：以 Electron/Tauri WebView 替代 `Spark`  
- **不做**：把索引/插件生命周期长期放在 C# 内

---

## 4. 逻辑分层

```
┌─────────────────────────────────────────┐
│ Presentation   搜索 UI / 设置 / 托盘菜单   │
├─────────────────────────────────────────┤
│ Application    命令路由 / 会话 / 插件编排   │
├─────────────────────────────────────────┤
│ Domain         索引模型 / 排序 / 权限 / 插件清单 │
├─────────────────────────────────────────┤
│ Infrastructure Win32 API / SQLite / IPC / FS │
└─────────────────────────────────────────┘
```

| 层 | 模块示例 |
|----|----------|
| Presentation | `ui_bridge`, 窗口状态机, 主题 |
| Application | `router`, `session`, `plugin_runtime` |
| Domain | `Query`, `Candidate`, `PluginManifest`, `Permission` |
| Infrastructure | `hotkey`, `pipe`, `sqlite_fts`, `shell_execute` |

---

## 5. 核心子系统

### 5.1 热键与窗口（Hotkey & Window）

- **默认热键**：`Alt+Space`（与常见 Cmd+Space 心智对齐；Windows 无 Cmd）
- **设置可改**：至少支持改为 `Ctrl+Space`，并允许自定义其它组合（需检测冲突）
- 使用 `RegisterHotKey`（或低层钩子作备选，默认不用全局钩子以降低杀软误报）
- 主窗口**预创建**，首次启动后隐藏，唤起时 `Show + SetForegroundWindow` 系列处理
- 处理：全屏独占、管理员/非管理员焦点差异、多显示器、DPI 感知（Per-Monitor V2）
- 改键流程：先 `UnregisterHotKey` → 注册新键 → 失败则回滚并提示冲突

**状态机（简化）：**

```
Hidden ──hotkey/tray──► Showing ──ready──► Active
   ▲                                      │
   └──────── esc / 失焦 / 执行完成 ─────────┘
```

### 5.2 检索与索引（Indexer）

**数据源（内置）：**

- 开始菜单 / 已安装应用
- PATH 可执行文件（可选）
- 用户配置的常用目录文件
- 插件注册的命令与关键字
- 历史记录与收藏（加权）

**存储：**

- 元数据与全文：`SQLite + FTS5`
- 热数据（最近/高频）：内存倒排或前缀索引
- 增量更新：文件系统监听（`ReadDirectoryChangesW`）+ 定时对账

**查询流水线：**

```
输入 ─► 规范化 ─► 内置索引查询 ─► 插件关键字匹配 ─► 融合排序 ─► 流式返回 UI
              └─ 可取消（新输入作废旧查询）
```

### 5.3 路由与排序（Router / Ranker）

- 无前缀：综合搜索（应用 > 文件 > 插件命令 > 历史）
- 有前缀/关键字：直达插件（如 `g ` = Google）
- 排序信号：匹配度、使用频率、时间衰减、用户固定置顶、插件优先级

### 5.4 插件管理（Plugin Manager）

- 扫描 `plugins/` 目录与已安装包
- 解析 `plugin.json`，校验签名（可选）与权限
- 维护插件进程池：空闲超时销毁、崩溃自动标记禁用
- 安装/卸载/启用/禁用/更新

### 5.5 配置与数据目录

```
%APPDATA%/<AppName>/
  config.toml
  data.db
  logs/
  plugins/
  cache/
```

便携模式：与 exe 同级的 `data/` 目录优先。

---

## 6. 插件体系架构

### 6.1 插件类型

| 类型 | 运行方式 | 适用场景 | 性能 |
|------|----------|----------|------|
| **Native Out-of-Process** | 独立 exe | OCR、截图、重 IO、任意语言 | 隔离好，有 IPC 成本 |
| **WASM In-Process** | Wasmtime 沙箱 | 纯逻辑、转换、轻计算 | 启动快、无 UI |
| **UI Extension**（二期） | 插件提供结果视图协议 | 复杂自定义面板 | 按需 |

一期以 **独立进程插件 + 内置命令** 为主，WASM 作轻量扩展。

### 6.2 插件包结构

```
my-plugin/
  plugin.json          # 清单（必须）
  plugin.exe           # 或 main.wasm
  icon.png
  README.md
  permissions/         # 可选说明
```

### 6.3 插件生命周期

```
Discovered → Installed → Enabled → Spawned → Ready → Busy → Idle → Stopped
                                ↘ Crashed → Quarantined
```

### 6.4 与宿主交互方式

- 插件**不直接**画主搜索窗（主窗由宿主 UI 统一）
- 插件通过协议返回：`List` 结果项 / `Action` 执行结果 / `Push` 通知
- 需要独立窗口的能力（如截图选区）由插件自己创建顶层窗，但需声明权限

---

## 7. IPC 架构

### 7.1 传输

| 通道 | 用途 |
|------|------|
| Named Pipe（主） | 控制面：请求/响应、事件 |
| Shared Memory（辅） | 大数据：缩略图、大列表 payload |
| stdout/stdin（仅调试） | 开发期简易模式 |

Pipe 名示例：`\\.\pipe\<app>.host.<session>`

### 7.2 协议风格

- JSON-RPC 2.0 文本帧，或 length-prefixed MessagePack（正式版偏二进制）
- 所有调用带 `id`、`plugin_id`、`timeout_ms`
- 支持 `cancel` 通知以中止长任务

### 7.3 消息方向

```
UI ──► Host ──► Plugin
 ▲       │         │
 └───────┴─────────┘  事件：results partial / progress / log
```

详细方法见 [DESIGN.md](./DESIGN.md) 协议章节。

---

## 8. 安全与权限

| 机制 | 说明 |
|------|------|
| 权限声明 | `clipboard`, `fs_read`, `fs_write`, `net`, `shell`, `screenshot`, `global_hook` |
| 用户授权 | 首次使用高危权限弹窗确认 |
| 进程隔离 | 默认独立进程，工作目录受限 |
| 可选代码签名 | 商店/官方源强制；本地开发可旁路 |
| 资源限制 | CPU/内存软限制、请求超时、并发上限 |

---

## 9. 性能架构约束

| 路径 | 约束 |
|------|------|
| 热键 → 窗口可见 | 不分配大对象、不碰磁盘、不启插件 |
| 默认搜索 | 仅内存/SQLite，主线程外执行，UI 只收结果 |
| 插件搜索 | 仅匹配到关键字或用户选中后才 spawn/调用 |
| 隐藏 | 停止渲染、可 trim working set；索引线程继续低优运行 |
| 日志 | 默认 info 异步；热路径禁止同步写盘 |

**性能预算（目标）：**

| 指标 | 目标 |
|------|------|
| 常驻 RSS | < 80MB（无插件活跃） |
| 唤起 P50 / P99 | < 30ms / < 50ms |
| 首结果 | < 30ms（本地 1 万条级） |
| 插件冷启动 | < 300ms（可后台预热常用插件） |

---

## 10. 技术栈

> **完整定稿与否决项以 [TECH_STACK.md](./TECH_STACK.md) 为准。** 下表为摘要。

| 组件 | 选型 | 说明 |
|------|------|------|
| 宿主 | Rust edition 2021+ | `tokio`, `thiserror`/`anyhow`, `windows` crate |
| DB | `rusqlite` + FTS5 | 索引持久化 |
| IPC | Named Pipe + JSON-RPC | `serde_json`；可迁 MessagePack |
| WASM | `wasmtime` | **P2** 轻插件 |
| UI | **C# + WinUI 3** | Windows App SDK；MVVM 可选 Toolkit |
| UI↔Host | 双进程 IPC | UI 无权威索引状态 |
| 构建 | Cargo workspace + .NET csproj | `crates/*` + `ui/Spark.UI` |
| 安装 | MSIX 或 Inno Setup | 实现前二选一 |

---

## 11. 仓库与 crate 划分

```
spark/
  Cargo.toml                 # Rust workspace
  crates/
    host/                    # spark-host
    core/                    # 领域模型、排序、清单（可单测）
    ipc/                     # 协议类型与编解码
    index/                   # 索引引擎
    plugin-manager/          # 发现/启停/权限
    sdk/                     # 插件 Rust SDK
  ui/
    Spark.UI/                # C# WinUI 3 → Spark.exe
  plugins/
    echo/                    # 示例插件
  brand/
  docs/
    TECH_STACK.md            # 技术栈定稿
    ARCHITECTURE.md
    DESIGN.md
    FEATURES.md
  ui-prototype/              # HTML 原型（非生产）
  scripts/
```

---

## 12. 部署架构

```
安装包
  ├─ spark-host.exe      # 自启、单实例、热键/托盘
  ├─ Spark.exe           # C# WinUI 主窗（可由 host 拉起）
  ├─ resources/          # 含 brand 图标
  ├─ plugins/ builtin/
  └─ uninstall
```

- 单实例：`Mutex` + 二次启动转发给已有实例  
- 自启：注册表 `HKCU\...\Run` 或任务计划程序  
- 更新：静默下载 → 替换 → 优雅重启 host

---

## 13. 可观测性

- 结构化日志（`tracing`）按模块过滤
- 本地性能计数：唤起耗时、查询耗时、插件 RPC 耗时
- 崩溃转储：host 与 plugin 分离收集
- 默认**不**上传；用户可选手动导出诊断包

---

## 14. 演进路线（架构视角）

| 阶段 | 架构交付 |
|------|----------|
| P0 MVP | 单进程 host+UI 线程，热键，本地应用搜索，无第三方插件 |
| P1 | 插件协议 + 独立进程 + SDK + 示例插件 |
| P2 | FTS 文件索引、权限系统、插件市场本地包格式 |
| P3 | WASM 轻插件、UI 进程分离、预热与更激进内存策略 |
| P4 | 签名源、自动更新、诊断与性能仪表盘 |

---

## 15. 风险与对策

| 风险 | 对策 |
|------|------|
| 焦点抢夺失败 | 多策略 SetForeground + 文档化管理员提权一致性 |
| 杀软拦截热键/注入 | 避免全局键盘钩子；签名；最小权限 |
| 插件生态碎片 | 稳定 IPC + 多语言 SDK（先 Rust，再 C# 模板） |
| UI 质感不足 | WinUI Fluent；关键页原生，复杂页慎用 WebView |
| SQLite 锁 | 单写多读、查询只读连接、索引构建另库切换 |

---

## 16. 相关文档

- [TECH_STACK.md](./TECH_STACK.md) — **技术栈定稿**（C# UI 决策、否决项）
- [DESIGN.md](./DESIGN.md) — 协议、数据模型、模块接口、里程碑
- [FEATURES.md](./FEATURES.md) — 功能与验收
- [UI_PROTOTYPE.md](./UI_PROTOTYPE.md) — 原型与 WinUI 映射
