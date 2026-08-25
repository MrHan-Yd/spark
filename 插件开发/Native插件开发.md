# Native 插件开发

> 状态：**运行时已实现**（2026-08-21）
> Wire 协议版本：`API_VERSION = 1`（`spark-ipc`）
> 清单版本：`api_version: 2`
> 面向：Rust / 原生语言开发者
> 完整契约见 [插件开发规范.md](./插件开发规范.md)

---

## 0. 实现现状（必读）

**Native 插件运行时已落地**：host 侧 spawn 插件进程 + stdin/stdout 管道 RPC，SDK 侧 `run_loop` 阻塞分发。内置 `echo` 示例端到端可跑（编译 exe 放入插件目录即可触发 `echo <文本>` 体验回显+复制）。

当前状态：

| 部分 | 状态 |
|------|------|
| 通信协议数据结构（`QueryParams`/`InvokeParams`/`QueryResult`/`InvokeResult`/`PluginMethod`） | ✅ `crates/ipc/src/protocol.rs` |
| 帧编解码（4 字节小端长度前缀 + UTF-8 JSON） | ✅ `crates/ipc/src/frame.rs`（`read_frame`/`write_frame`，16 MiB 上限） |
| 插件 SDK trait（`Plugin::query/invoke`） + `run_loop` | ✅ `crates/sdk/src/lib.rs`（`run_loop` + 可单测 `dispatch_request`） |
| host 侧 spawn native 进程 + 管道通信循环 | ✅ `crates/plugin-manager/src/native.rs`（`NativeRuntime`：懒启动/常驻/崩溃重建/超时/shutdown） |
| 关键字前缀路由（`mode:"list"`） | ✅ `PluginManager::find_native_match`（保留原大小写） |
| `echo` 示例插件 | ✅ `plugins/echo/src/main.rs` 用 `run_loop`，编译产物 `spark-plugin-echo.exe` |

**已知 v1 权衡**：native query 是**阻塞 RPC（5s 超时）在 host 锁内**调用，重型插件超时/崩溃自动降级为空结果，不阻断搜索主流程。`mode: "page"`（native 自建窗口）仍属二期，未实现。

---

## 1. 是什么

**Native 插件 = 一个独立 exe，通过 stdin/stdout 的 JSON-RPC 与 Spark host 通信。**

- 插件目录含 `plugin.json` + 编译好的 `xxx.exe`（exe 由开发者随插件分发，Spark 不编译）。
- host 按需 spawn 插件进程，通过管道收发 JSON-RPC 帧。
- 插件返回 `Candidate` 结果项融入主搜索列表（`mode: list`），或自建顶层窗口（`mode: page`，二期）。
- 适合：OCR、截图选区、大量 IO、特定语言生态、需要任意原生 OS 能力的重型插件。

普通前端工具请走 [WebView插件开发.md](./WebView插件开发.md)。

---

## 2. 目录结构

```
my-native-plugin/
  plugin.json            # 清单（必须）
  my-native-plugin.exe   # 可执行文件（必须，main 指向它）
  icon.png               # 图标（可选）
  README.md              # 说明（可选）
```

**关键区别于 webview**：发布态的 native 插件**自带编译好的 exe**，用户安装时连 exe 一起落盘，host 只负责调用。开发者负责把目标平台（一期仅 Windows x64）的 exe 构建好放进插件目录。

### 落盘位置

| 类型 | 位置 |
|------|------|
| 正式安装 | `<Spark 安装目录>/plugins/<id>/` |
| 开发模式 | 开发者自选本地目录（不拷贝） |
| 插件状态 | `<data_dir>/plugins-state.json` |

---

## 3. 清单 `plugin.json`

Native 插件用**旧清单格式**（`commands` + `keywords`），向后兼容。UTF-8 无 BOM。

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `id` | string | 是 | 反向域名，全局唯一，必须含 `.` |
| `name` | string | 是 | 显示名 |
| `version` | string | 是 | 语义化版本 |
| `api_version` | number | 是 | 当前 `2` |
| `runtime` | string | 是 | 固定 `"native"` |
| `main` | string | 是 | exe 文件名，如 `spark-plugin-echo.exe` |
| `icon` / `author` / `description` / `homepage` | — | 否 | 元信息 |
| `permissions` | string[] | 否 | 能力白名单（native 通常用 OS 能力，权限模型二期细化） |
| `commands` | object[] | 是 | 命令描述（native 用 `commands`，**不是** webview 的 `features`），至少 1 个 |
| `keywords` | string[] | 否 | 触发关键字列表（兼容字段） |

### 3.1 `commands` 项

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `name` | string | 是 | 命令标识 |
| `title` | string | 是 | 列表项标题 |
| `subtitle` | string | 否 | 副标题 |
| `mode` | string | 是 | `list`（返回结果项融入主列表，一期）或 `page`（自建窗口，二期） |
| `prefix` | string | 否 | 触发前缀，如 `"echo "` |

### 3.2 完整示例

```json
{
  "id": "com.spark.echo",
  "name": "Echo",
  "version": "0.1.0",
  "api_version": 2,
  "runtime": "native",
  "main": "spark-plugin-echo.exe",
  "description": "示例插件：回显输入",
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
  "permissions": []
}
```

> 清单校验规则（`manifest.rs`）：native 插件必须 `commands` 非空；webview 插件必须 `features` 非空。两者不可混用。

---

## 4. 通信协议

**传输层**：stdin/stdout，**length-prefixed JSON-RPC**（每帧 = 4 字节小端 uint32 长度 + UTF-8 JSON body）。host 与插件是 1:1 进程对，host 拥有管道读写两端。

帧编解码实现见 `crates/ipc/src/frame.rs`（`read_frame`/`write_frame`，16 MiB 上限，干净 EOF 返 `Ok(None)`）。

### 4.1 方法（`PluginMethod`，`crates/ipc/src/protocol.rs`）

| 方法 | 方向 | 说明 |
|------|------|------|
| `plugin.initialize` | host → 插件 | 启动握手，传插件 id/权限/运行环境；插件回 `PluginInitializeResult { plugin_id, sdk_version }` |
| `plugin.shutdown` | host → 插件 | 优雅退出（可带 id 等 ack，或作 notification 直接退出） |
| `plugin.query` | host → 插件 | 查询：根据用户输入返回候选结果项 |
| `plugin.invoke` | host → 插件 | 执行：用户选中某结果项的动作 |
| `plugin.cancel` | host → 插件 | 取消进行中的 query（notification，无 `id`，不回响应） |

每条请求带 `id`（host 侧递增 u64），插件返回对应 `id` 的 `JsonRpcResponse`；`cancel` 为 notification（无 `id`，不回帧）。host 侧**校验响应 id 与请求 id 一致**，不符即视为协议错误丢弃进程重建——故插件 stdout 必须纯净，不要回多余/自发帧。

### 4.2 数据结构

**`plugin.query` 请求参数 `QueryParams`**：
```jsonc
{ "text": "echo hello", "limit": 50 }
```

**`plugin.query` 响应 `QueryResult`**：
```jsonc
{
  "items": [
    {
      "id": "echo:hello",
      "title": "hello",
      "subtitle": "Echo · Enter 复制",
      "target": null,
      "icon": null,
      "score": 1.0,
      "source": "plugin",
      "plugin_id": "com.spark.echo",
      "actions": [
        { "id": "copy", "title": "复制", "is_default": true, "target": null }
      ]
    }
  ],
  "partial": false
}
```
（`items` 元素即 `spark_core::Candidate`。）

**`plugin.invoke` 请求参数 `InvokeParams`**：
```jsonc
{ "item_id": "echo:hello", "action_id": "copy", "text": "hello" }
```

**`plugin.invoke` 响应 `InvokeResult`**（`tag = "type"`）：
| type | 字段 | 说明 |
|------|------|------|
| `close` | `message?` | 执行后关主窗，可选提示 |
| `keep` | `message?` | 执行后保留主窗，可选提示 |
| `copy_text` | `text` | 复制文本到剪贴板 |
| `open_url` | `url` | 用系统默认浏览器打开 URL |
| `show_error` | `message` | 显示错误提示 |
| `confirm` | `message` | 不可逆操作二次确认；用户确认后以 `action_id="confirm"` 重新 invoke |

Wire 协议版本常量 `API_VERSION = 1`（`crates/ipc/src/protocol.rs`），破坏性变更时 bump。

---

## 5. Rust 开发（`spark-sdk`）

Rust 开发者引用 `spark-sdk` crate，实现 `Plugin` trait。

### 5.1 `Plugin` trait（`crates/sdk/src/lib.rs`）

```rust
pub trait Plugin {
    fn id(&self) -> &str;
    fn query(&mut self, params: QueryParams) -> QueryResult;
    fn invoke(&mut self, params: InvokeParams) -> InvokeResult;
}
```

辅助函数：
```rust
spark_sdk::single_item(item: Candidate) -> QueryResult   // 单条结果
spark_sdk::empty_result() -> QueryResult                  // 空结果
spark_sdk::parse_params<T: DeserializeOwned>(Value) -> Result<T, _>
spark_sdk::sdk_version() -> &'static str
```

### 5.2 最小插件骨架

```rust
use spark_core::{Action, Candidate, Source};
use spark_ipc::{InvokeParams, InvokeResult, QueryParams, QueryResult};
use spark_sdk::{single_item, Plugin};

struct Echo;

impl Plugin for Echo {
    fn id(&self) -> &str { "com.spark.echo" }

    fn query(&mut self, params: QueryParams) -> QueryResult {
        let text = params.text;
        single_item(Candidate {
            id: format!("echo:{text}"),
            title: text.clone(),
            subtitle: Some("Echo · Enter 复制".into()),
            target: None,
            icon: None,
            score: 1.0,
            source: Source::Plugin,
            actions: vec![Action {
                id: "copy".into(),
                title: "复制".into(),
                is_default: true,
                target: None,
            }],
            plugin_id: Some(self.id().into()),
        })
    }

    fn invoke(&mut self, params: InvokeParams) -> InvokeResult {
        InvokeResult::CopyText { text: params.text }
    }
}
```

### 5.3 `Cargo.toml`

```toml
[package]
name = "spark-plugin-echo"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "spark-plugin-echo"
path = "src/main.rs"

[dependencies]
spark-sdk = { path = "../../crates/sdk" }
spark-core = { path = "../../crates/core" }
spark-ipc = { path = "../../crates/ipc" }
serde_json = "1"
```

### 5.4 main：运行时

`run_loop` 由 `spark-sdk` 提供，负责：帧编解码、`plugin.initialize` 握手、`query`/`invoke`/`cancel` 分发、`shutdown` 退出。stdout 必须纯净协议帧，日志走 stderr。

```rust
fn main() {
    let mut plugin = Echo;
    // 阻塞：读 stdin 帧 → dispatch query/invoke → 写 stdout 帧。
    if let Err(e) = spark_sdk::run_loop(&mut plugin) {
        eprintln!("spark-plugin-echo: run_loop exited: {e}");
        std::process::exit(1);
    }
}
```

---

## 6. 其他语言

非 Rust 插件自行实现协议帧：

1. **帧格式**：每条消息 = 4 字节小端 uint32 长度（不含自身）+ UTF-8 JSON。从 stdin 读长度再读 body，向 stdout 写长度再写 body。stderr 留给插件自用日志（host 不解析）。
2. **消息体**：标准 JSON-RPC 2.0（`{ jsonrpc, id, method, params }` 请求 / `{ jsonrpc, id, result|error }` 响应 / `{ jsonrpc, method, params }` 通知）。
3. **方法与结构**：照本文 §4 的方法名与 `QueryParams`/`QueryResult`/`InvokeParams`/`InvokeResult` 字段名（snake_case）。
4. **stdout 必须纯净**：只写协议帧，不要写任何调试打印（会破坏帧解析）；日志走 stderr。
5. **刷新缓冲**：每帧写完立即 flush stdout，避免 host 阻塞等待。

---

## 7. 生命周期与错误处理

```
host spawn 插件进程（带 plugin.initialize）
        │
        ▼
插件就绪，host 缓存进程
        │ 用户输入匹配关键字
        ▼
host 发 plugin.query → 插件返回 QueryResult.items 融入主列表
        │ 用户选中某项动作
        ▼
host 发 plugin.invoke → 插件返回 InvokeResult（close/keep/copy_text/...）
        │
        ├─ 空闲超时 / host 退出 → plugin.shutdown → 进程退出
```

- 插件进程 crash：host 检测到管道断开，下次 query 时重新 spawn；用户无感。
- query 超时：host 发 `plugin.cancel`（notification），插件应尽快中止并丢弃该 `id` 的响应。
- 插件内部错误：响应 `{ error: { code, message } }`，不要 panic 退出进程。

---

## 8. 打包与发布

1. 在目标平台（一期 Windows x64）编译 exe：`cargo build --release -p spark-plugin-echo`。
2. 把产物 `target/release/spark-plugin-echo.exe` 复制进插件目录（与 `plugin.json` 同级，文件名与 `main` 字段一致）。
3. 整个目录压成 zip，扩展名改 `.spark-plugin`（同 webview）。
4. 用户在设置 → 插件 → 从本地安装；exe 随目录一起落盘到 `<安装目录>/plugins/<id>/`。

> 与 webview 不同：native 插件分发**必须**带上目标平台预编译 exe，Spark host 不负责编译，只负责 spawn 调用。
> native 插件 exe 同样走内容清单签名（Ed25519，随仓库 CI 自动签名），不叠加 Windows Authenticode。详见 [插件签名规范.md](./插件签名规范.md)。

### 8.1 通过插件市场发布

1. 编译好 exe，在插件仓库中创建 `插件名/版本号/` 目录，放入 `plugin.json` + exe。
2. 更新仓库根目录 `registry.json`，添加该插件条目（`runtime: "native"`）。
3. 推送后用户即可在 Spark 设置 → 插件 → 插件市场浏览并一键安装。
4. native 插件 exe 体积大时，建议在 `version.url` 填预打包 zip 地址（GitHub Release asset），避免每次下载整个仓库 zipball。

详见 [插件市场与仓库.md](./插件市场与仓库.md) §6 发布流程。

---

## 9. 完整示例：echo

仓库内置 `plugins/echo`（Rust native 示例）。

- 清单见 §3.2。
- 源码 `plugins/echo/src/main.rs`（用 `spark_sdk::run_loop`，见 §5.4）。
- `Cargo.toml` 见 §5.3，是 workspace 成员（`Cargo.toml` members 含 `plugins/echo`）。

**可运行**：`cargo build --release -p spark-plugin-echo`，把产物 `target/release/spark-plugin-echo.exe` 复制进插件目录（与 `plugin.json` 同级，文件名与 `main` 字段一致）。开发模式启动 host 指向项目根 `plugins/`，输入 `echo <文本>` 即触发回显候选，回车复制。

## 10. 路线图

| 阶段 | 内容 |
|------|------|
| 当前 | 协议结构 + SDK trait + 帧编解码 + `run_loop` + host spawn/管道循环；echo 可跑 |
| 二期 | 插件市场支持（官方/自定义仓库、registry.json 索引、一键安装/更新）；`mode: page` 自建顶层窗口（需 `window.create` 权限）；native 权限模型细化 |
| 三期 | 插件市场签名校验、自动检查更新、跨平台产物声明 |
| 四期 | WASM 轻插件、多仓库源、插件依赖声明 |

> 完整阶段划分见 [插件开发规范.md](./插件开发规范.md) §13 路线图。

---

## 11. 安全约束

1. Native 插件拥有完整 OS 能力，权限模型比 webview 更需谨慎（二期细化）。
2. host 对 native 插件进程做超时/资源约束；stderr 不回传用户。
3. stdout 必须纯净协议帧，任何杂质都会破坏解析。
4. 市场上架（二期）将强制代码签名；本地开发免签名。
5. 不得收集用户隐私上传未声明用途的服务器。

---

> 本指南面向原生插件开发者。完整字段 schema、IPC 方法表、版本兼容策略见 [插件开发规范.md](./插件开发规范.md)。