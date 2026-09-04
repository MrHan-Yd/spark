# 搜索体验性能台账（主页面输入卡顿 + 结果出现慢）

> 目标：① 主页搜索框打字无卡顿感；② 按键到结果出现的延迟显著缩短。
> 规约：一次只做一个任务，做完跑校验（Rust 改动 `cargo fmt` + `cargo test --workspace`，UI 改动编译通过），勾掉后再做下一个。
> 全部完成后走 Code Auditor 审计闭环，PASSED 才交付。
> 前置台账：`docs/PERF-STARTUP.md`（冷启动/热键唤醒，已完成），本文聚焦搜索查询链路。

## 背景（2026-09-02 评估结论）

查询链路：按键 → `OnQueryChanged` → `ScheduleRefresh`（120ms 防抖，IME 组词挂起）→ `RefreshResultsAsync` → IPC `host.query` → host 全局锁内查询（索引 + 插件关键字 + native RPC）→ JSON 回包 → `ApplyResults`（Reset 批量替换 + 容器重绑 + 高亮重建 + 图标补齐）。

评估结论：卡顿不是单一原因，是"UI 线程同步文件 IO"+"host 锁内阻塞 RPC"+"固定防抖/渲染churn"叠加。逐项证据如下。

## 已定位问题（按严重度）

### 🔴 P0-A｜每个结果项绑定都在 UI 线程同步写日志（输入卡顿头号嫌疑）

- `MainWindow.xaml.cs:1740` `OnTitleDataContextChanged` 里有调试期遗留的 `App.Log("HL", …)`。
- `App.Log` 是**同步 `File.AppendAllText`**（`App.xaml.cs:71`，AppendAllText 在 :79），每次调用还同步执行 `Directory.CreateDirectory`，无轮转/大小上限。
- 打字时结果 id 几乎每键都变 → `ApplyResults` 走 `_items.ReplaceAll`（单次 Reset，`MainWindow.xaml.cs:1699`）→ 所有可见容器重新绑定 → **每个容器一次同步文件开/写/关**。一屏 10~15 项 × 每键。
- 证据：本机 `%LOCALAPPDATA%\Spark\ui-crash.log` **2026-09-02 本机快照**：1911 行中 HL 行占 303 行，同一毫秒内连写多条；文件无限增长，越用越慢。
- 同路径问题：IME 组词开始/结束打点（:181、:186）；host 断连时每次按键 `QueryFallback` 异常也走文件（`HostIpcClient.cs:267`，同类还有 :276、:285）。

### 🔴 P0-B｜native 插件 RPC 在 host 全局锁内阻塞执行（结果慢的最坏情况根因）

- `crates/host/src/app.rs:110-125`：`HostApp::search` 在**持有全局 `host.lock()`** 时调用 `native_query`（锁覆盖范围见 `ipc_server.rs:309-319` 的 host.query 分支，`SharedHost = Arc<Mutex<HostApp>>`）。
- 首次查询的锁内最坏阻塞远大于单次 RPC 超时，链路为三段（`plugin-manager/src/native.rs`）：
  1. `NativeProcess::spawn`（:79）——进程创建，**时长无界、不设防**；
  2. `initialize` 握手 RPC（:131，spawn 内 :119 同步执行）——走 `rpc()`，超时同为 **5 秒**（:59 `DEFAULT_RPC_TIMEOUT`）；
  3. `query` RPC（:158）——再 ≤5 秒。
  即最坏 = 无界 spawn + ~10s RPC。注意 `native.rs:238` 的 1s 是 `shutdown()` 的退出轮询 deadline，与 spawn 无关，不能当作首查上界。
- 已有进程时每次查询也要锁内等 ≤5s（:193 `recv_timeout`）。
- **超时处置放大问题**：`with_proc`（:354 起）对任何超时/IO 失败**一律丢弃进程、下次重 spawn**（:363-366；首次失败同样丢弃 :373-375）。任何超时调整都必须同步考虑该语义。
- 触发条件：输入命中某 native 插件关键字（`find_native_match`）。命中后每次按键查询都在锁内等插件回话；期间**所有 IPC（下一次查询/invoke/设置）全部排队**。
- 架构违规：热路径原则——阻塞 RPC 不应出现在持锁的搜索路径上。

### 🟡 次要因素

1. **防抖 120ms 固定延迟**（`MainWindow.xaml.cs:60` `QueryDebounceMs`）：按键到发查询的固定延迟，叠加 IPC 往返构成体感延迟主体。
2. **host 锁内全量扫描 + 每候选 4 次 `to_lowercase` 分配**（`crates/index/src/memory.rs`：过滤阶段 3 次 :87/:91/:96，打分循环第 4 次 :109）：几百应用时 <1ms 尚可，但每键重复分配，纯浪费。
3. **插件候选图标 UI 线程同步读文件**：`PluginIconLoader.Load` 的 `File.Exists` + SVG 头部 2KB 嗅探（`PluginIconLoader.cs:27-37`），每个新插件项一次。注意 Load 的调用面不止搜索结果：设置列表、插件窗口同样走此路径（见文件头注释）。

## 任务清单

### [x] ① 清理热路径同步日志（P0-A）
- [x] 删除 `OnTitleDataContextChanged` 的 `App.Log("HL", …)`（直接移除）
- [x] 顺带移除 `Root.PointerPressed` 临时诊断日志（同款热路径调试遗留，IME kick 验证已完成）
- [x] IME 组词开始/结束打点改为状态翻转时记一次（`if (_composing) return;` 门闸，防 TSF 连发）
- [x] `QueryFallback`：同一连接代只记一条（`HostIpcClient.LogFallbackOnce`，按 `_connectionGeneration` 去重，重连成功复位）
- [x] `App.Log` 重构：5MB 轮转为 `.old`；顺带加 `LogGate` 锁修掉并发 AppendAllText 互相截断丢行的老问题（PERF-STARTUP 遗留 🟡 项）
- **校验**：`dotnet build -c Debug -p:Platform=x64` 0 警告 0 错误 ✓

### [x] ② 防抖自适应（体感延迟）
- [x] `QueryDebounceMs` 120 → 80；新增 `_lastScheduledQuery` 跟踪：上次调度为空而本次非空（首字符）立即查询不走防抖；组词中早退不更新该字段，上屏补查自然按首字符语义立即发
- [x] IME 挂起语义保持（`_composing` 早退路径未动）；`_queryGen` 守卫未绕过
- **校验**：UI 编译通过 ✓；中文组词行为依赖既有事件门闸，待人工回归

### [x] ③ native RPC 移出全局锁（P0-B，host 架构修正）

**③-a 前置结构改动（已落地：专职线程形态）**：
- [x] `NativeRuntime` 状态整体迁入专职线程（`native::spawn_runtime_thread`），`NativeRuntimeHandle`（Clone=共享发送端，join 在 `Arc<Mutex<Option<JoinHandle>>>`）经消息通信（`RuntimeMsg`：QuerySearch/QueryBlocking/Invoke/Warm/WarmDone/ShutdownPlugin/ShutdownAll）
- [x] 锁序纪律（已写入 native.rs 模块注释）：native 线程从不取 host 锁；host 线程等待 native 应答一律在 host 锁外；invoke 路径例外地持锁等待（native 线程对 host 锁无依赖，单向等待无死锁环，ipc_server host.invoke 注释标注）

**③-b 搜索路径超时语义**：
- [x] `SEARCH_RPC_TIMEOUT = 300ms`；`NativeProcess::rpc` 超时参数化；**超时保留进程**（`RpcFailure::Timeout` 与 `Fatal` 分类：Timeout 保留、Fatal 才丢弃），与 invoke 路径"失败即杀"（`with_proc` 原语义不变）区分
- [x] **搜索路径不懒启动**（③-b 推荐口径）：`query_for_search` 无进程即 `NotReady`；spawn+握手（无界）放独立预热线程，完成后经 `WarmDone` 消息归位 runtime 进程表（竞态：已有进程丢弃后到者）；initialize 握手随之彻底离开搜索路径
- [x] spawn 无界问题：预热线程化后搜索路径不再同步等待 spawn ✓
- [x] rpc id 过滤语义变更：不匹配响应由"杀进程"改为"忽略并继续等"——搜索超时放弃请求后，迟到旧应答滞留通道，按旧逻辑会误杀保留的进程；只收精确匹配 id，防串号保证不变（代码注释已说明）

**③-c partial 传递链（新建）**：
- [x] `HostApp::search_prep(text, client_limit)` 返回 `SearchPrep { hits, native: Option<NativeSearchRequest>, limit }`；`ipc_server` host.query 改三段式：锁内 prep → 锁外 `execute()` → 合并截断；`partial` 不再硬编码 false
- [x] UI 承接：`RefreshResultsAsync` 收到 `Partial=true` 后延迟 600ms 补查一次（`PartialRequeryQuery` 同词去重 + `_queryGen` 守卫，用户继续输入自然作废）

**③-d 语义保持与节流评估**：
- [x] invoke 路径维持懒启动 + 5s + 失败即杀（`RuntimeMsg::Invoke`，句柄 15s 盖帽防悬挂）；`native_shutdown_all` 收割线程；覆盖更新前的 `shutdown_plugin` 改 `shutdown_plugin_sync`（同步等待退出，.exe 可覆盖语义保持）
- [x] 节流评估结论：不加 host 侧节流——300ms 上限 + 锁外执行已锁定单次查询成本；同一 UI 连接的查询在 IPC client 线程串行化，最坏排队 = 键数×300ms，UI `_queryGen` 丢弃过期结果后最后一发即收敛；额外节流的复杂度不成比例
- **校验**：`cargo fmt` ✓；`cargo test -p spark-plugin-manager`（66，含新增 upsert 缓存回归在 index）✓；`cargo test --workspace` 142 全过 ✓
- **待人工验证**：慢响应 native 插件的"查询不被拖死/进程不被每键重启/partial 补查恰好一次"需装真实 echo 插件端到端跑（本机无 native 插件，逻辑已由单测覆盖）

### [x] ④ MemoryIndex 小写缓存（host 查询热路径零分配）
- [x] `MemoryIndex` 条目改为 `IndexedApp { cand, title_lc, sub_lc, target_lc }`，upsert 时算一次；search 单趟过滤+打分全取缓存（含打分循环第 4 次 to_lowercase），打分语义与旧实现逐档一致（0.35/0.25/0.12/0.05）
- [x] history 兜底路径：`candidates_title_containing` 把标题过滤下沉到 Candidate 构造**之前**（未命中条目不再付整串克隆代价）；`HistoryEntry` 加 `title_lc: Mutex<(src, lc)>` 自校验缓存（标题比对不一致才重算，任何改标题路径不会读到过期值；Mutex 而非 RefCell——须满足 SearchIndex 的 Send+Sync；MappedMutexGuard 本工具链不可用故内联临界区）
- [x] 新增测试：`upsert_replaces_keeps_lowercase_fresh`、`search_surfaces_history_title_hits`
- **校验**：`cargo test -p spark-index` 40 全过 ✓

### [x] ⑤ PluginIconLoader 文件嗅探挪后台
- [x] `Load` 移除，新增 `LoadAsync`：`File.Exists` + SVG 头部嗅探在 `Task.Run` 后台，位图/SVG 对象构造回 UI 线程（线程亲和）
- [x] 全部调用方适配：搜索候选 `LoadIconAsync`、收藏卡 `LoadFavIconAsync`（本就 async，改 await）；插件窗口 `TrySetIcon` → `LoadTitleIconAsync`（fire-and-forget，降级链保持）；设置页 `PluginRowVm`（ctor fire-and-forget + `IconImage` 补 `private set` + INPC 通知三属性翻转）
- **校验**：UI 编译 0 警告 0 错误 ✓；三调用面（搜索候选/设置页/插件窗口）图标回归待人工确认

### [x] ⑥ 全量校验 + 审计闭环
- [x] `cargo fmt --check` 通过；`cargo test --workspace` 143 全过（含新增 3 个回归测试）
- [x] `dotnet build ui/Spark.UI/Spark.UI.csproj -c Debug -p:Platform=x64` 0 警告 0 错误
- [x] 冒烟：`spark-host --query explorer` 返回正确结果；daemon + 命名管道 JSON-RPC 两条 host.query（命中/未命中）回包正确、`partial:false` 如实；修复轮后 `--query` 复跑正常退出（EXIT 0）
- [x] Code Auditor 全量 diff 审计：首轮 NEEDS_FIX（2 🔴：预热无在途去重进程风暴 / rpc 无绝对 deadline 可被帧流饿死致退出 join 悬挂）→ 修复 → 回归 **PASSED**

## 回归审计残余项（2026-09-03 PASSED 轮登记，均有界边缘，不阻断）

- 🟡 R1 覆盖更新竞态：`shutdown_plugin_sync` 完成后、安装换名进行中，在途 WarmDone 可能把旧 exe 进程插回进程表占用新版本 id 槽位直到 host 退出（至多一个旧版进程、临时旧版结果、.bak 由启动期清理兜底）。后续可在 insert_warm 注释/台账补充；如需根治，覆盖更新前撤销在途预热标记。
- 🟡 R2 join 有界注释措辞略乐观：严格上还应加 ShutdownAll 之前在途消息耗时（QueryBlocking 懒启动最坏 = spawn + 5s + 5s），仍有界。
- 🟡 R3 partial 补查"部分回删重打同词"（abc→ab 且 ab 曾 partial）仍可能少一次补查，下一键自愈。

## 待人工回归（功能性验证，单测未覆盖）

- [ ] 装真实 native echo 插件端到端：查询不被拖死、进程不被每键重启、partial 补查恰好一次、慢响应插件 300ms 后 UI 600ms 补查拿到结果
- [ ] 图标三调用面回归：搜索候选 / 设置页插件列表 / 插件窗口标题栏（含 SVG 与缺失文件占位）
- [ ] 中文输入法组词回归：组词期间不查询、上屏立即补查一次、候选窗正常弹出

## 实测基线（优化前，待补）

- [ ] 打字跟手度：`ui-crash.log` 每键新增行数（2026-09-02 本机快照：~10-15 行/键）
- [ ] 查询 RTT：可在 `RefreshResultsAsync` 临时打点测 host.query 毫秒数（优化后移除或降频）
- [ ] 带 native 插件前缀输入时的最坏查询延迟（理论最坏 = 无界 spawn + ~10s RPC，实测待补）

## 实施备注

- 工作区尚有 PERF-STARTUP 轮次的未提交 WIP（见 git status），在其上叠加，不回退。
- ①② 是两个独立回归面：① 纯 UI 低风险可先发；② 顺带；③ 涉及锁结构重构 + 超时语义 + partial 新链路，单独一轮改+审；④⑤ 各自独立。
- 审计记录（2026-09-03 首轮 NEEDS_FIX 已修订）：更正 native.rs:238 证据归属；任务③重写为 a/b/c/d 四段（锁结构前置、超时不杀进程、partial 传递链与 UI 补查为新建机制、invoke 语义保持）；④ 纳入打分循环缓存；校验命令统一带 `-p:Platform=x64`。
- 审计记录（2026-09-03 代码首轮 NEEDS_FIX 已修复回归）：
  - 🔴 Issue #1 预热无在途去重 → `NativeRuntime` 新增 `warming: HashSet<String>`：Warm 到达时已就绪或在途都忽略；WarmDone 归位（成败都清）先移除标记，失败保证可重试；warm 线程创建失败不标记；孤儿进程归属结论：WarmDone 归位时插件可能已卸载（runtime 线程不回查插件表），与既有"卸载不关进程"行为同款留表至 host 退出 shutdown_all 收割，后续搜索不会命中。
  - 🔴 Issue #2 rpc 无绝对 deadline → `rpc()` 改 `deadline = Instant::now() + timeout`，每次 recv 按剩余时间：自发帧再密也无法把总等待拖过 timeout；`shutdown_all` join 最坏 = 一个 rpc 超时 + 每进程 shutdown ≤1s，有界。
  - 🟡 #1 兜底覆盖 51+ 名次确认为有意改善，补测试 `history_fallback_covers_beyond_first_50`；🟡 #2 `--query` 退出前补 `host.shutdown()` 收割 native 进程；🟡 #3 空查询时复位 PartialRequeryQuery（覆盖删光重打同词）；🟡 #4 ShowLauncher 显式复位 `_lastScheduledQuery`；🟡 #6 query_search send 失败补 warn；🟡 #8 过期注释 120ms→80ms。#5（LogFallbackOnce 纳秒级双记）审计已认可接受；#7 核对旧 ipc_server 语义完全一致（`params.limit.min(max_results)`），非行为变更。