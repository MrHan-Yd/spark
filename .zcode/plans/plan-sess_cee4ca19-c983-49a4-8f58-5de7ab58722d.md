# native 插件 = 纯应用模型（移除 list 支持，页面化）

## 模型定义（本方案的不变量）

1. **native 插件不再支持 `commands`/`keywords`**——manifest 校验直接拒绝声明，插件从根上无法进搜索框、无法出现在主窗口，主窗口行为零影响。
2. **native 插件必须有 `page`**（HTML 页面），只能从插件卡片「打开」/「调试」进入；页面即插件的全部 UI（WebView 展示，Spark 现有依赖零新增）。
3. **exe 生命周期 = 页面生命周期**：懒启动（打开页面后首次 RPC 才 spawn + 握手）；**关闭页面 → host 优雅关停该插件进程**——“不打开就是不用”字面成立。
4. 搜索链路的 native 整合代码全部移除（路由、300ms 搜索查询、预热、搜索结果合并）。
5. exe 是纯逻辑引擎，页面经 host 转发 RPC（`plugin.page`）调用它；协议里 query/invoke/cancel 方法保留但 host 不再向 native 插件发送。

## 改动清单

### 1. crates/ipc/src/protocol.rs
- `PluginMethod` 增加 `Page`（`"plugin.page"`）；新增 `PluginPageParams { method: String, args: Value(默认) }`（`deny_unknown_fields`）。
- 新增 UI→host 命令 `host.plugin.page_closed { id }`（关窗通知，用于关停 native 进程）。

### 2. crates/plugin-manager/src/manifest.rs
- native 校验：声明 `commands`/`keywords` → **报错**（提示 native 已改为纯应用模型，请用 page）；`page` 字段**必填**，校验相对路径（拒绝绝对路径与 `..`）、`.html` 后缀。webview 校验不变。

### 3. crates/plugin-manager/src/native.rs — 大幅精简
- **移除**搜索路径全部机制：`query_for_search`、`NativeSearchRequest`、`RuntimeMsg::QuerySearch/Warm/WarmDone`、`SEARCH_RPC_TIMEOUT`、`SearchQueryError`、预热去重集合；`find_native_match` 移除（含其测试）。
- 保留：spawn + CREATE_NO_WINDOW、initialize 握手、`rpc`（5s）、shutdown/生命周期、专职 runtime 线程。
- 新增 `RuntimeMsg::PageCall { info, method, args, reply }` + `NativeRuntimeHandle::page_call`（懒启动 + 5s + 失败即杀，语义同原 invoke）。

### 4. crates/plugin-manager/src/lib.rs
- `open()`：native 分支——enabled + page 文件存在 → `main_abs` = 页面绝对路径；否则报 `"plugin {id} has no page"`。
- 新增 `native_page_call(id, method, args)`；`native_spawn_info` 保留供 PageCall 用。
- **移除** `native_search_request` / `native_query_blocking` / `native_invoke`。
- 列表 DTO 增加 `has_page: bool`（native 恒 true；webview=有 mode:page feature）。

### 5. crates/host/src
- `app.rs`：`search`/`search_prep` 移除 native 合并段（`SearchPrep` 去掉 `native` 字段，ipc_server 同步）；`invoke()` 移除 native 分支；`plugin_api` 新增 `"rpc"` 分支（仅 native 放行，转发 `native_page_call`，data=Value；webview → `UNAVAILABLE`）。
- `ipc_server.rs`：分发 `host.plugin.page_closed` → `app.plugin_page_closed(id)` → native 进程优雅关停（已有 `shutdown_plugin_sync`）。

### 6. crates/sdk/src/lib.rs
- `Plugin` trait 增加 `fn page(&mut self, params: PluginPageParams) -> Value { Value::Null }` 默认实现；`dispatch_request` 增加 `plugin.page` 分支。query/invoke 保留（协议兼容）。

### 7. plugins/echo — 转纯页面（端到端验证载体）
- plugin.json：去 `commands`/`keywords`，`api_version` 升 2，加 `"page": "page.html"`。
- 新增 page.html：`spark.rpc("echo", …)` 回显 + `spark.window.setTitle`。
- src/main.rs：实现 `page()` 回显 method/args。

### 8. ui/Spark.UI
- `Assets/plugin.preload.js`：`spark` 增加 `rpc(method, args)`（capability="rpc"）。
- 插件 DTO 增加 `HasPage`；插件卡片 HasPage 时显示「打开」按钮（「调试」旁），点击 `PluginOpenAsync + OpenOrFocus`；`MainWindow.xaml.cs:3864` 文案改"（插件未启用或无可打开页面）"。
- `PluginWindow.OnClosed`：native 插件时调用 `host.plugin.page_closed`（新增 HostIpcClient 方法）触发进程关停。

### 9. 文档
- `插件开发/Native插件开发.md`：重写为纯应用模型——page 必填、无 commands、卡片入口、生命周期（开页启动/关窗退出）、plugin.page RPC、spark.rpc、调试方式；list 模式章节标注移除。
- `插件开发/插件开发规范.md` §10/§13、`WebView插件开发.md` spark.* 表同步。

### 10. 测试
- **移除/重写**：native 搜索路由、搜索合并、native invoke、warming 相关测试；app.rs 测试 fixture（fy/trl/zn/tr 等 native commands 插件）改为 page 模型或删除断言。
- **新增**：manifest 拒绝 native commands/keywords、page 必填与路径校验；`open()` native 有/无 page；`native_page_call` 转发与非 native 拒绝；SDK `plugin.page` dispatch；`host.plugin.page_closed` 关停。
- **门禁**：`cargo fmt` + `cargo test --workspace`。

## 明确后果（已确认接受）
- 现有 local-search v0.1.0（声明 find 命令）在新模型下**加载失败**、从插件列表消失，待插件仓库侧改造为纯应用后恢复。
- `spark-host --query` 诊断不再含 native 结果；主搜索窗口从此与 native 插件完全无关。

## 手动 e2e
`cargo build -p spark-plugin-echo --release` → 拷 exe 到 plugins/echo/ → dev_host.ps1 → 插件卡片出现「打开」，页面 rpc 回显成功；关闭窗口后 host 日志确认插件进程退出；搜索框任意输入无任何 echo 痕迹。

按 AGENTS.md 工作流：完成后提交 **Code Auditor** 审计，修复至 PASSED 再交付。