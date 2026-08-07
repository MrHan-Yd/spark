# Spark — 设计文档

> 产品：**Spark**  
> 配套：[ARCHITECTURE.md](./ARCHITECTURE.md) · **[TECH_STACK.md](./TECH_STACK.md)（技术栈定稿）**  
> 技术决策：**Rust Host + C# WinUI 3 UI** + 开放插件（独立进程；WASM = P2）  
> 优先级：性能 > 稳定性 > 可扩展 > 开发速度

---

## 1. 产品设计摘要

### 1.1 核心用户流程

1. 用户按下全局热键（默认 `Alt+Space`；设置中可改为 `Ctrl+Space` 等）
2. 搜索窗在当前鼠标/活动屏中央偏上出现，输入框已聚焦
3. 输入即时出结果（应用、文件、命令、插件）
4. `Enter` 执行默认动作；`Ctrl+Enter` 等可绑次要动作
5. `Esc` 或失焦隐藏；执行成功后按策略隐藏或保持

### 1.2 功能范围（按阶段）

| 功能 | P0 | P1 | P2 |
|------|----|----|-----|
| 全局热键 / 托盘 / 单实例 | ✓ | ✓ | ✓ |
| 应用搜索与启动 | ✓ | ✓ | ✓ |
| 历史与收藏 | ✓ | ✓ | ✓ |
| 插件协议 + 本地安装 | | ✓ | ✓ |
| 文件索引 FTS | | △ | ✓ |
| 权限与设置 UI | | ✓ | ✓ |
| WASM 插件 | | | ✓ |
| 插件市场 / 更新 | | | ✓ |

---

## 2. 关键技术决策（ADR 摘要）

| ID | 决策 | 结论 | 原因 |
|----|------|------|------|
| ADR-1 | 宿主语言 | **Rust** | 性能、内存安全、适合常驻 |
| ADR-2 | UI 框架 | **WinUI 3** | 原生观感、DPI、动画；热路径不走 Web |
| ADR-2b | UI 语言 | **C#**（定稿） | 体验迭代快；性能热路径在 Rust，无需 C++ UI |
| ADR-3 | 插件默认形态 | **独立进程** | 崩溃隔离、语言无关 |
| ADR-4 | 轻插件 | **WASM**（P2） | 快、可沙箱 |
| ADR-5 | IPC | **Named Pipe + JSON-RPC**（可迁 MessagePack） | Windows 原生、调试友好 |
| ADR-6 | 索引 | **SQLite FTS5 + 内存热缓存** | 可靠、够快 |
| ADR-7 | 全局钩子 | **默认不用** | 降低杀软误报 |
| ADR-8 | 主界面技术 | **禁止 Electron/Tauri 主路径** | 内存与唤起不符合性能第一 |
| ADR-9 | 产品名 / 品牌 | **Spark**；Logo A2+B1 | 四芒星 + 石墨黑白 |

---

## 3. 数据模型

### 3.1 领域对象

```rust
// 概念模型（示意）

struct Query {
    text: String,
    session_id: Uuid,
    limit: u32,
    cancellation: CancellationToken,
}

struct Candidate {
    id: String,              // 稳定 id，用于去重与历史
    title: String,
    subtitle: Option<String>,
    icon: IconRef,           // path / plugin-relative / system
    score: f32,
    source: Source,          // BuiltinApp | File | History | Plugin(id)
    actions: Vec<Action>,
    plugin_id: Option<String>,
}

struct Action {
    id: String,              // "open" | "copy" | "reveal" | custom
    title: String,
    is_default: bool,
    payload: serde_json::Value,
}

enum IconRef {
    Path(PathBuf),
    Glyph(String),
    Plugin { plugin_id: String, name: String },
}
```

### 3.2 插件清单 `plugin.json`

```json
{
  "id": "com.example.echo",
  "name": "Echo",
  "version": "0.1.0",
  "api_version": 1,
  "main": "plugin.exe",
  "runtime": "native",
  "description": "示例插件",
  "author": "example",
  "keywords": ["echo", "demo"],
  "commands": [
    {
      "name": "echo",
      "title": "Echo",
      "subtitle": "回显输入",
      "mode": "list",
      "prefix": "echo "
    }
  ],
  "permissions": ["none"],
  "min_host_version": "0.1.0",
  "os": ["windows"]
}
```

**字段说明：**

| 字段 | 说明 |
|------|------|
| `id` | 全局唯一，反向域名 |
| `runtime` | `native` \| `wasm` |
| `main` | 入口文件 |
| `commands[].mode` | `list` 返回列表 / `action` 直接执行 / `view` 二期 |
| `commands[].prefix` | 触发前缀；空表示可参与全局融合（需谨慎） |
| `permissions` | 权限枚举列表 |
| `api_version` | 协议主版本，不兼容则拒绝加载 |

### 3.3 配置 `config.toml`（示意）

```toml
[general]
language = "zh-CN"
launch_on_startup = true
hide_on_focus_lost = true

[hotkey]
# 默认 Alt+Space（对应 macOS 常见 Cmd+Space 心智）
# 设置页可改为 Ctrl+Space 或其他组合
toggle = "Alt+Space"
# 可选预设（UI 快捷选项，非同时生效）
# presets = ["Alt+Space", "Ctrl+Space"]

[search]
debounce_ms = 50
max_results = 50
min_score = 0.1

[plugins]
extra_dirs = []
idle_ttl_secs = 120
max_concurrent = 4

[index]
enable_files = false
file_paths = []
rebuild_on_start = false

[ui]
theme = "system"
window_width = 720
max_visible_items = 9
```

### 3.4 数据库表（草案）

```sql
-- 应用/命令/文件统一条目可分表，此处示意

CREATE TABLE items (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,          -- app|file|command|history
  title TEXT NOT NULL,
  subtitle TEXT,
  target TEXT NOT NULL,        -- 启动路径或协议
  icon TEXT,
  plugin_id TEXT,
  updated_at INTEGER NOT NULL
);

CREATE VIRTUAL TABLE items_fts USING fts5(
  title, subtitle, target,
  content='items', content_rowid='rowid'
);

CREATE TABLE usage_stats (
  item_id TEXT PRIMARY KEY,
  use_count INTEGER NOT NULL DEFAULT 0,
  last_used_at INTEGER NOT NULL
);

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

---

## 4. IPC 协议设计

### 4.1 帧格式

**一期（调试友好）：** 一行一条 JSON（NDJSON），UTF-8。  
**二期：** `u32 LE length + MessagePack payload`。

### 4.2 Host → Plugin 方法

| Method | 说明 | 何时调用 |
|--------|------|----------|
| `plugin.initialize` | 传入 host 能力、数据目录、权限令牌 | 进程启动后 |
| `plugin.shutdown` | 优雅退出 | 空闲回收/卸载 |
| `plugin.query` | 查询列表 | 用户输入匹配该插件 |
| `plugin.invoke` | 执行动作 | 用户选中并确认 |
| `plugin.cancel` | 取消进行中的 query/invoke | 新输入或超时 |

#### `plugin.query` 请求

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "plugin.query",
  "params": {
    "request_id": "uuid",
    "command": "echo",
    "text": "hello",
    "limit": 20
  }
}
```

#### `plugin.query` 响应

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "items": [
      {
        "id": "echo:hello",
        "title": "hello",
        "subtitle": "Echo",
        "score": 1.0,
        "actions": [
          { "id": "copy", "title": "复制", "is_default": true }
        ]
      }
    ],
    "partial": false
  }
}
```

`partial: true` 时允许后续 `plugin.results` 通知推送增量。

#### `plugin.invoke` 请求

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "plugin.invoke",
  "params": {
    "request_id": "uuid",
    "command": "echo",
    "item_id": "echo:hello",
    "action_id": "copy",
    "text": "hello"
  }
}
```

#### `plugin.invoke` 响应

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "type": "close",
    "message": "已复制"
  }
}
```

`type` 枚举：`close` | `keep` | `open_url` | `copy_text` | `show_error` | `run_command`（受权限约束）。

### 4.3 Plugin → Host 方法 / 通知

| Method / Event | 说明 |
|----------------|------|
| `host.log` | 写宿主日志 |
| `host.clipboard.get/set` | 需 `clipboard` 权限 |
| `host.fs.read` 等 | 需对应 fs 权限 |
| `host.notify` | 系统通知 |
| `host.storage.get/set` | 插件私有 KV（沙箱路径） |
| `plugin.results`（notify） | 流式补充查询结果 |
| `plugin.progress`（notify） | 长任务进度 |

### 4.4 UI ↔ Host

UI 与 Host 使用同一套 IPC 风格（若同进程则改为 channel）。

| Method | 说明 |
|--------|------|
| `ui.show` / `ui.hide` | 显示隐藏 |
| `ui.set_query` | 外部设置搜索词 |
| `ui.results` | 推送候选列表 |
| `host.query` | UI 上报输入变化 |
| `host.invoke` | UI 上报执行 |
| `host.get_config` / `set_config` | 设置 |

### 4.5 超时与取消

| 操作 | 默认超时 |
|------|----------|
| `initialize` | 3s |
| `query` | 1s（可配置；超时返回已有部分结果） |
| `invoke` | 15s |
| 空闲无调用 | `idle_ttl_secs` 后 kill 进程 |

取消：Host 发 `plugin.cancel`，插件应中止并返回；若无响应则杀进程。

---

## 5. 模块详细设计

### 5.1 `host` 启动序列

```
1. 解析 CLI / 便携模式
2. 单实例 Mutex；若已存在 → 发送 "toggle" 后退出
3. 初始化 tracing、config、data dir
4. 打开 SQLite，必要时迁移 schema
5. 启动 tokio runtime
6. 注册热键、托盘
7. 预创建 UI（窗口 hidden）
8. 后台：加载插件清单（不全部 spawn）
9. 后台：增量索引 / 应用枚举
10. 进入消息循环 + async 驱动
```

### 5.2 查询流水线（Host）

```
on_input(text):
  cancel previous QuerySession
  session = new QuerySession(text)
  
  // 并行
  local = index.search(text, limit)
  plugin_hits = match_commands_by_prefix_or_keyword(text)
  
  ranked = rank(merge(local, plugin_hits.placeholders))
  ui.push(ranked)  // 首屏尽快
  
  for cmd in plugin_hits.need_rpc:
    spawn_or_get(plugin)
    async items = plugin.query(...)
    ui.patch(merge ranked)
```

**规则：**

- 纯本地结果必须在首包返回，不等插件
- 仅 `prefix` 命中或 score 极高的全局插件才触发 RPC
- 输入防抖 `debounce_ms`（默认 50，可对空词更短）

### 5.3 排序算法（初版）

```
score = text_match_score * w1
      + frequency_score * w2
      + recency_score * w3
      + source_boost          # app > command > file
      + exact_prefix_boost
      + pinned_boost
```

- `text_match_score`：前缀 > 子序列 > FTS 相关性  
- 频率与时间用简单对数与指数衰减即可，避免复杂 ML

### 5.4 插件进程管理

```rust
struct PluginProcess {
    id: String,
    child: Child,
    ipc: PipeClient,
    last_active: Instant,
    state: Ready | Busy | Crashed,
}
```

- 进程池：按 `plugin_id` 最多 1 个实例（一期）
- 崩溃：记录次数，连续 N 次自动 Disable 并通知用户
- 工作目录：`%APPDATA%/.../plugins/<id>/` 数据目录与安装目录分离

### 5.5 权限模型

```text
none | clipboard | fs_read | fs_write | net | shell | screenshot | notify | storage
```

- 清单声明 ⊆ 用户已授权，否则 RPC 返回 `PermissionDenied`
- `shell` 与 `fs_write` 为高危，设置中单独开关
- Host 侧强制鉴权，插件侧 SDK 仅辅助

### 5.6 热键与焦点（Windows 细节）

1. `RegisterHotKey` 收消息
2. 显示窗口：`ShowWindow` + 多策略前台激活  
   - 若失败：任务栏闪烁兜底 + 日志
3. 与 UI 同完整性级别（尽量避免 admin 宿主 + 非 admin 前台冲突）；需要提权工具时用独立 elevated helper
4. DPI：清单声明 `PerMonitorV2`
5. 全屏游戏：允许用户配置“禁用热键”或“仅桌面会话”

---

## 6. UI 设计要点

### 6.1 主窗口

- 无边框、圆角、阴影（WinUI）
- 宽度默认 720，高度随结果条数变化（最大 N 条）
- 区域：搜索框 | 结果列表 | 底部提示（快捷键/来源）
- 主题：跟随系统 Light/Dark，支持强调色

### 6.2 交互

| 按键 | 行为 |
|------|------|
| `↑↓` | 选择结果 |
| `Enter` | 默认动作 |
| `Ctrl+Number` | 快捷选第 N 项（可选） |
| `Esc` | 清空或隐藏 |
| `Tab` | 动作菜单 / 补全前缀（可配） |
| `Alt+Enter` | 打开文件位置等次要动作 |

### 6.3 性能相关 UI 约束

- 虚拟化列表（只渲染可见行）
- 图标异步加载 + 占位 + 缓存
- 动画可关（辅助功能/低配）
- 禁止在输入回调里做同步磁盘 IO

### 6.4 设置页

- 热键（默认 Alt+Space，可改 Ctrl+Space 等）、启动、主题
- 索引路径
- 插件列表（启用/权限/卸载）
- 关于与诊断导出

---

## 7. SDK 设计（Rust 优先）

### 7.1 插件作者最小代码（示意）

```rust
use launcher_sdk::{Plugin, QueryCtx, InvokeCtx, Item, Response};

struct Echo;

impl Plugin for Echo {
    fn id(&self) -> &str { "com.example.echo" }

    fn query(&mut self, ctx: QueryCtx) -> Response {
        Response::items(vec![Item::new(ctx.text()).action_copy()])
    }

    fn invoke(&mut self, ctx: InvokeCtx) -> Response {
        Response::copy_and_close(ctx.text())
    }
}

fn main() {
    launcher_sdk::run(Echo);
}
```

### 7.2 SDK 职责

- 建立与 Host 的 pipe 连接（Host 通过 env 传入 pipe 名）
- 反序列化请求、调用 trait、写回响应
- 提供 `storage` / 日志 helper
- 处理 `cancel` 与超时协作

### 7.3 多语言

| 语言 | 方式 |
|------|------|
| Rust | 官方 SDK crate |
| C# | 模板 + 小型 IPC helper lib |
| 其他 | 实现 JSON-RPC 即可，提供协议文档 |

---

## 8. 目录与文件布局（落地）

```
repo/
  Cargo.toml
  crates/
    core/                 # Query, Candidate, rank, manifest
    ipc/                  # 协议 struct + codec
    index/                # sqlite + scanner
    plugin-manager/
    host/                 # 二进制入口
    sdk/
    ui-bridge/
  ui/Spark.UI/            # C# WinUI 3 → Spark
  plugins/
    echo/
      plugin.json
      Cargo.toml          # 产出 plugin.exe
  docs/
    ARCHITECTURE.md
    DESIGN.md
  resources/
    icons/
  scripts/
    dev_run.ps1
    package.ps1
```

### 环境变量（插件进程）

| 变量 | 说明 |
|------|------|
| `LAUNCHER_PIPE` | 命名管道路径 |
| `LAUNCHER_PLUGIN_ID` | 插件 id |
| `LAUNCHER_DATA_DIR` | 插件可写数据目录 |
| `LAUNCHER_HOST_VERSION` | 宿主版本 |
| `LAUNCHER_API_VERSION` | 协议版本 |

---

## 9. 错误处理

| 场景 | 行为 |
|------|------|
| 插件超时 | 返回部分结果 + 标记慢插件；UI 不卡死 |
| 插件崩溃 | 隔离、记录、可选自动重启 1 次 |
| 索引损坏 | 备份坏库、重建、提示用户 |
| 热键注册失败 | 托盘提示冲突，引导改键 |
| 权限拒绝 | 结果项提示「需要授权」并跳转设置 |

错误码（IPC）：

```text
1  ParseError
2  MethodNotFound
3  InvalidParams
4  PermissionDenied
5  Timeout
6  Cancelled
7  Internal
8  NotInitialized
```

---

## 10. 测试策略

| 类型 | 范围 |
|------|------|
| 单元测试 | `core` 排序、清单解析、权限集合 |
| 集成测试 | 假插件进程 + pipe 往返 |
| 性能基准 | 唤起、1万条搜索、FTS 查询（criterion） |
| 手动清单 | 多 DPI、多屏、全屏、管理员、输入法 |

**性能门禁（CI 可先做库级 bench）：**

- `rank` 1 万候选 < 5ms  
- FTS 查询 P95 目标写入 bench 文档  

---

## 11. 里程碑与交付物

### M0 — 工程骨架（约 1 周）

- workspace、`host` 空窗/托盘、配置加载
- 文档与插件清单 schema 定稿

### M1 — 可用搜索（约 2–3 周）

- 热键唤起 + 应用枚举搜索 + 启动
- 历史加权
- 基础设置

### M2 — 插件可用（约 2–3 周）

- IPC 全流程、`sdk`、echo 插件
- 安装本地文件夹插件、启停、崩溃隔离

### M3 — 体验与索引（约 2–4 周）

- WinUI 打磨、虚拟列表、主题
- 文件 FTS、权限 UI
- 安装包与自启

### M4 — 生态预备

- WASM runtime、C# 模板、签名策略、自动更新

---

## 12. 开发规范（简要）

- Host 热路径：禁止 `unwrap` 直接崩溃；插件 IPC 错误必须可恢复
- 禁止在 UI 线程做磁盘/网络
- 所有跨进程结构变更必须升 `api_version` 或做兼容字段
- 日志不得记录剪贴板全文与密钥
- 第三方插件默认无 `net`，需显式授权

---

## 13. 开放问题（待决）

| 问题 | 选项 | 状态 / 建议 |
|------|------|-------------|
| UI 用 C# 还是 C++ | C# / C++/WinRT | **已定：C#**（TECH_STACK） |
| Host 与 UI 进程 | 双进程 / 同进程 | **已定方向：双进程**；Host 可 spawn UI |
| 协议 JSON 还是 MessagePack | 先 JSON | 按 ADR-5 |
| 是否支持全局无 prefix 插件查询 | 性能敏感 | 默认关闭，插件申请 `contribute_global` |
| 商店形态 | 仅本地包 / 以后 HTTP 源 | P4 再做 |
| 安装器 / 许可证 / 包 ID | 见 TECH_STACK §10 | 不挡骨架开发 |

---

## 14. 附录：最小 `plugin.json` 校验规则

1. `id` 匹配 `^[a-z0-9]+(\.[a-z0-9]+)+$`
2. `api_version == host.supported`
3. `main` 文件存在且后缀与 `runtime` 一致
4. `permissions` 均在已知枚举内
5. `commands` 至少 1 个；`name` 插件内唯一

---

## 15. 附录：成功标准（一期）

- [ ] 冷启动后热键唤起 P99 < 50ms（中端 PC 测量）
- [ ] 无插件时 RSS < 80MB
- [ ] 应用搜索可用，历史 dual 加权正确
- [ ] 至少 1 个第三方风格插件（独立 exe）完整安装运行
- [ ] 插件崩溃后 host 与热键仍可用
- [ ] WinUI 主界面在 100%/150%/200% DPI 下布局正常
