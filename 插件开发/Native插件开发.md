# Native 插件开发

> 状态：**纯应用模型已实现**（2026-09-04 改版；旧 `commands`/list 模式已移除；同日补上 `features` 关键字搜索入口）
> Wire 协议版本：`API_VERSION = 1`（`spark-ipc`）
> 清单版本：`api_version: 2`
> 面向：Rust / 原生语言开发者
> 完整契约见 [插件开发规范.md](./插件开发规范.md)

---

## 0. 实现现状（必读）

**Native 插件是"纯应用"模型**：

- **exe 永不直接应答搜索**——旧 `commands`（`find xx` 式：exe 在搜索链路里实时应答、往结果列表注入动态结果）已移除且清单校验直接拒绝。
- **搜索框入口与 webview 同构**——native 可声明 `features`（**仅 `mode: "page"`**）：关键字在搜索框产出候选（含真前缀建议），回车/点击打开的正是插件的页面窗口。exe 不参与搜索过程，只服务于页面。
- **页面即全部 UI**——插件必须提供 HTML 页面（`page` 字段），Spark 用与 webview 插件同款的插件窗口（WebView2）代开；入口 = 搜索框关键字候选 或 设置 → 插件卡片的「打开」按钮（开发者模式另有「调试」）。
- **exe 生命周期 = 页面生命周期**——懒启动（页面首次 RPC 才 spawn + 握手）；**关闭页面 host 即优雅关停进程**。"不打开就是不用"字面成立。
- 页面 JS 经 `spark.rpc(method, args)` 调用 exe 的原生逻辑（host 转发 `plugin.page` RPC），exe 也可以只用页面本地能力（db/clipboard 等）不实现任何自定义方法。

| 部分 | 状态 |
|------|------|
| 帧编解码（4 字节小端长度前缀 + UTF-8 JSON） | ✅ `crates/ipc/src/frame.rs`（16 MiB 上限） |
| `plugin.page` RPC（页面 → host → exe） | ✅ `crates/ipc/src/protocol.rs` + `crates/plugin-manager/src/native.rs` |
| 插件 SDK trait（`Plugin::page`） + `run_loop` | ✅ `crates/sdk/src/lib.rs` |
| host 侧 spawn / 懒启动 / 崩溃重建 / 关窗关停 | ✅ `crates/plugin-manager/src/native.rs`（`NativeRuntime`） |
| features 关键字进搜索框（page 模式，同 webview 路由） | ✅ `crates/plugin-manager/src/lib.rs`（`find_keyword_match` 等） |
| UI 卡片「打开」/「调试」按钮 + `spark.rpc` | ✅ `ui/Spark.UI`（`PluginWindowHost` 复用 webview 窗口基建） |
| `echo` 示例插件（纯页面） | ✅ `plugins/echo/`（page.html + `page()` 回显 + `echo` 关键字） |

**已移除**：`commands`/list 路由、搜索结果合并、native invoke、进程预热——exe 不再向搜索框返回任何动态结果（旧版插件声明 `commands` 会被清单校验直接拒绝，加载失败）。

---

## 1. 是什么

**Native 插件 = 一个独立 exe + 一个 HTML 页面，是 Spark 里的一个"小应用"。**

- 插件目录含 `plugin.json` + 编译好的 `xxx.exe`（`main` 指向）+ HTML 页面（`page` 指向）。
- 用户从插件卡片「打开」页面；页面跑在 WebView2 里，可调用 `spark.*` 能力。
- 页面需要原生计算/IO 时，经 `spark.rpc` → host → 管道 RPC 调用 exe；exe 天然拥有完整 OS 能力。
- 适合：设置页复杂的管理型工具、OCR、截图、大量 IO、需要任意原生 OS 能力的重型插件。

只要前端逻辑的请走 [WebView插件开发.md](./WebView插件开发.md)（webview 插件与 native 的关键字路由、页面窗口完全同构，差异只在页面背后有没有 exe）。

---

## 2. 目录结构

```
my-native-plugin/
  plugin.json            # 清单（必须）
  my-native-plugin.exe   # 可执行文件（必须，main 指向它；页面不用 rpc 时可暂不放）
  page.html              # 页面入口（必须，page 指向它）
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

UTF-8 无 BOM。**native 不允许旧 `commands`/`keywords` 字段**（exe 直接应答搜索的 list 模式，声明即校验失败、拒绝加载）；`features` 允许，但仅限 `mode: "page"`。

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `id` | string | 是 | 反向域名，全局唯一，必须含 `.` |
| `name` | string | 是 | 显示名 |
| `version` | string | 是 | 语义化版本 |
| `api_version` | number | 是 | 当前 `2` |
| `runtime` | string | 是 | 固定 `"native"` |
| `main` | string | 是 | exe 文件名，如 `spark-plugin-echo.exe` |
| `page` | string | 是 | 页面入口，相对路径 `.html`（禁止绝对路径与 `..`） |
| `features` | array | 否 | 搜索框入口（同 webview；**仅 `mode: "page"`**，`type: "keyword"`） |
| `icon` / `author` / `description` / `homepage` | — | 否 | 元信息 |
| `permissions` | string[] | 否 | 能力白名单（`spark.rpc` 不需要权限，见 §4.3） |
| `window` | object | 否 | 页面窗口规格（宽高/可缩放/置顶/边框，同 webview） |
| `preload` | string | 否 | 自定义预加载脚本（同 webview，可选） |

### 3.1 完整示例

```json
{
  "id": "com.spark.echo",
  "name": "Echo",
  "version": "0.2.0",
  "api_version": 2,
  "runtime": "native",
  "main": "spark-plugin-echo.exe",
  "page": "page.html",
  "description": "示例插件：纯应用模型（页面 + spark.rpc 回显）",
  "features": [
    {
      "type": "keyword",
      "keyword": "echo",
      "title": "Echo",
      "subtitle": "示例插件：页面 + spark.rpc 回显",
      "mode": "page",
      "placeholder": "输入任意文本…"
    }
  ],
  "permissions": []
}
```

> 清单校验规则（`manifest.rs`）：native 声明 `commands`/`keywords` 即报错；`features` 仅接受 `mode: "page"`（`list` 报错）；`page` 必填且须为相对 `.html` 路径。

---

## 4. 通信协议

**传输层**：stdin/stdout，**length-prefixed JSON-RPC**（每帧 = 4 字节小端 uint32 长度 + UTF-8 JSON body）。host 与插件是 1:1 进程对，host 拥有管道读写两端。

帧编解码实现见 `crates/ipc/src/frame.rs`（`read_frame`/`write_frame`，16 MiB 上限，干净 EOF 返 `Ok(None)`）。

### 4.1 方法（`PluginMethod`，`crates/ipc/src/protocol.rs`）

| 方法 | 方向 | 说明 |
|------|------|------|
| `plugin.initialize` | host → 插件 | spawn 后握手，传插件 id/权限/协议版本；插件回 `{ plugin_id, sdk_version }` |
| `plugin.page` | host → 插件 | 页面 `spark.rpc(method, args)` 的转发；插件回**自定义 JSON**（原样回传页面） |
| `plugin.shutdown` | host → 插件 | 优雅退出（页面关闭 / 覆盖更新 / host 退出时发送） |
| `plugin.query` / `plugin.invoke` / `plugin.cancel` | — | 旧 list 模式方法，**协议保留但 host 不再发送** |

每条请求带 `id`（host 侧递增 u64），插件返回对应 `id` 的 `JsonRpcResponse`。host 校验响应 id 与请求 id 一致，不符即忽略——stdout 必须纯净，不要回多余/自发帧。

### 4.2 `plugin.page` 数据结构

**请求参数 `PluginPageParams`**（页面 JS `spark.rpc('get_config', {...})` 转发而来）：
```jsonc
{ "method": "get_config", "args": { "key": "value" } }
```

**响应 result**：插件自定义 JSON，原样回传页面：
```jsonc
{ "drives": { "C:": true }, "max_results": 50 }
```

### 4.3 权限说明

`spark.rpc` **不设新权限**：native exe 本就拥有完整 OS 能力，页面与 exe 出自同一插件目录、同一信任级（与 `spark.db` 默认开放同理由）。`spark.*` 其余能力（clipboard/notify/db 等）沿用 webview 的声明+授权模型。

Wire 协议版本常量 `API_VERSION = 1`（`crates/ipc/src/protocol.rs`），破坏性变更时 bump。

---

## 5. Rust 开发（`spark-sdk`）

Rust 开发者引用 `spark-sdk` crate，实现 `Plugin` trait。

### 5.1 `Plugin` trait（`crates/sdk/src/lib.rs`）

```rust
pub trait Plugin {
    fn id(&self) -> &str;
    /// 旧 list 模式入口：host 不再调用，保留仅为协议兼容（给出空实现即可）。
    fn query(&mut self, params: QueryParams) -> QueryResult;
    fn invoke(&mut self, params: InvokeParams) -> InvokeResult;
    /// 页面 spark.rpc 的宿主；返回值原样回传页面 JS。默认返回 null。
    fn page(&mut self, params: PluginPageParams) -> serde_json::Value;
}
```

辅助函数：
```rust
spark_sdk::parse_params<T: DeserializeOwned>(Value) -> Result<T, _>   // args 反序列化
spark_sdk::sdk_version() -> &'static str
```

### 5.2 最小插件骨架

```rust
use serde_json::Value;
use spark_ipc::{InvokeParams, InvokeResult, PluginPageParams, QueryParams, QueryResult};
use spark_sdk::Plugin;

struct MyApp;

impl Plugin for MyApp {
    fn id(&self) -> &str { "com.example.myapp" }

    fn query(&mut self, _: QueryParams) -> QueryResult {
        spark_sdk::empty_result()   // host 不会调用
    }
    fn invoke(&mut self, _: InvokeParams) -> InvokeResult {
        spark_ipc::InvokeResult::Close { message: None }
    }

    fn page(&mut self, params: PluginPageParams) -> Value {
        match params.method.as_str() {
            "get_config" => serde_json::json!({ "max_results": 50 }),
            "set_config" => {
                // 写配置/重建索引等原生逻辑；失败回 JSON-RPC error（页面 Promise reject）
                serde_json::json!({ "ok": true })
            }
            other => serde_json::json!({ "error": format!("unknown method {other}") }),
        }
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

`run_loop` 由 `spark-sdk` 提供，负责：帧编解码、`plugin.initialize` 握手、`plugin.page` 分发、`shutdown` 退出。stdout 必须纯净协议帧，日志走 stderr。

```rust
fn main() {
    let mut plugin = MyApp;
    if let Err(e) = spark_sdk::run_loop(&mut plugin) {
        eprintln!("my-plugin: run_loop exited: {e}");
        std::process::exit(1);
    }
}
```

---

## 6. 其他语言

非 Rust 插件自行实现协议帧：

1. **帧格式**：每条消息 = 4 字节小端 uint32 长度（不含自身）+ UTF-8 JSON。从 stdin 读长度再读 body，向 stdout 写长度再写 body。stderr 留给插件自用日志（host 不解析）。
2. **消息体**：标准 JSON-RPC 2.0（`{ jsonrpc, id, method, params }` 请求 / `{ jsonrpc, id, result|error }` 响应 / `{ jsonrpc, method, params }` 通知）。
3. **只需实现三个方法**：`plugin.initialize` / `plugin.page` / `plugin.shutdown`（snake_case 字段见 §4.2）。
4. **stdout 必须纯净**：只写协议帧，不要写任何调试打印（会破坏帧解析）；日志走 stderr。
5. **刷新缓冲**：每帧写完立即 flush stdout，避免 host 阻塞等待。

---

## 7. 生命周期与错误处理

```
用户在插件卡片点「打开」→ UI 开 WebView2 页面窗口
        │  （exe 此时尚未启动）
        │  页面首次 spark.rpc(...)
        ▼
host 懒启动 exe + plugin.initialize 握手（最坏 ~5s，其后常驻）
        │  plugin.page 转发调用（5s 超时）
        ▼
用户关闭页面 → host 发 plugin.shutdown → 进程退出（1s 内不退则强杀）
```

- 插件进程 crash：host 检测到管道断开，下次 `plugin.page` 时重新 spawn；页面拿到错误后重试即可。
- page 调用超时/出错：host 返回 `"UNAVAILABLE: detail"` 形状错误，页面 `spark.rpc` 的 Promise reject（`error.code = "UNAVAILABLE"`）。
- 插件内部错误：响应 `{ error: { code, message } }`，不要 panic 退出进程。
- 页面应**每次改动即时保存**（经 rpc 写配置/写 db）：关窗即进程退出，没有"退出前存盘"钩子。

---

## 8. 打包与发布

1. 在目标平台（一期 Windows x64）编译 exe：`cargo build --release -p spark-plugin-echo`。
2. 把产物 `target/release/spark-plugin-echo.exe` 复制进插件目录（与 `plugin.json` 同级，文件名与 `main` 字段一致）。
3. 整个目录压成 zip，扩展名改 `.spark-plugin`（同 webview）。
4. 用户在设置 → 插件 → 从本地安装；exe 随目录一起落盘到 `<安装目录>/plugins/<id>/`。

> 与 webview 不同：native 插件分发**必须**带上目标平台预编译 exe，Spark host 不负责编译，只负责 spawn 调用。
> native 插件 exe 同样走内容清单签名（Ed25519，随仓库 CI 自动签名），不叠加 Windows Authenticode。详见 [插件签名规范.md](./插件签名规范.md)。

### 8.1 通过插件市场发布

1. 编译好 exe，在插件仓库中创建 `插件名/版本号/` 目录，放入 `plugin.json` + exe + page.html。
2. 更新仓库根目录 `registry.json`，添加该插件条目（`runtime: "native"`）。
3. 推送后用户即可在 Spark 设置 → 插件 → 插件市场浏览并一键安装。
4. native 插件 exe 体积大时，建议在 `version.url` 填预打包 zip 地址（GitHub Release asset），避免每次下载整个仓库 zipball。

详见 [插件市场与仓库.md](./插件市场与仓库.md) §6 发布流程。

---

## 9. 完整示例：echo

仓库内置 `plugins/echo`（Rust native 纯页面示例）。

- 清单：`page: "page.html"` + `features`（`echo` 关键字，page 模式）。
- 页面 `plugins/echo/page.html`：输入文本 → `spark.rpc('echo', {...})` → 展示 exe 回显的 JSON。
- 源码 `plugins/echo/src/main.rs`：`page()` 原样回显 `method`/`args`。

**可运行**：`cargo build --release -p spark-plugin-echo`，把产物 `spark-plugin-echo.exe` 复制进 `plugins/echo/`（与 `plugin.json` 同级）。开发模式启动 host 指向项目根 `plugins/`，设置 → 插件 → Echo 卡片 → 「打开」，在页面里发任意文本验证回显；关闭窗口后插件进程退出。

## 10. 路线图

| 阶段 | 内容 |
|------|------|
| 当前 | 纯应用模型：page 必填 + `plugin.page` RPC + `spark.rpc` + 卡片打开/关窗关停；echo 页面版可跑 |
| 二期 | 插件市场支持（官方/自定义仓库、registry.json 索引、一键安装/更新）；native 权限模型细化 |
| 远期 | `commands[].mode:"page"` 自建顶层原生窗口（需 `window.create` 权限；纯应用模型不满足的场景，如截图选区浮层） |
| 三期 | 插件市场签名校验、自动检查更新、跨平台产物声明 |
| 四期 | WASM 轻插件、多仓库源、插件依赖声明 |

> 完整阶段划分见 [插件开发规范.md](./插件开发规范.md) §13 路线图。

---

## 11. 安全约束

1. Native 插件拥有完整 OS 能力，权限模型比 webview 更需谨慎（二期细化）。
2. `spark.rpc` 是页面与自家 exe 之间的通道，不设权限（同源同信任）；host 对其他 `spark.*` 能力仍按"清单声明 + 用户授权"鉴权。
3. host 对 native 插件进程做超时约束（page 调用 5s、懒启动盖帽 15s）；stderr 不回传用户。
4. stdout 必须纯净协议帧，任何杂质都会破坏解析。
5. 市场上架将强制代码签名；本地开发免签名。
6. 不得收集用户隐私上传未声明用途的服务器。

---

> 本指南面向原生插件开发者。完整字段 schema、IPC 方法表、版本兼容策略见 [插件开发规范.md](./插件开发规范.md)。