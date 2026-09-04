# 启动性能优化台账（冷启动 + 热键唤醒）

> 目标：① 冷启动（点图标/开机自启到热键可用）显著提速；② 热键唤起首帧前的同步路径瘦身。
> 规约：一次只做一个任务，做完跑校验（Rust 改动 `cargo fmt` + `cargo test --workspace`，UI 改动编译通过），勾掉后再做下一个。
> 全部完成后走 Code Auditor 审计闭环，PASSED 才交付。

## 背景（2026-09-02 评估结论）

- **冷启动瓶颈**：UI 发布是 self-contained 全 JIT（无 R2R）；host `bootstrap` 同步做 Start Menu 全量索引（数百 .lnk COM 解析，几百 ms）+ 插件扫描，**串行阻塞热键注册**（热键在 `win_loop::run` 里才注册）。
- **唤醒链路**：热键 → `SetEvent`（命名事件）→ `ToggleWatcher` → `HandleToggle` → `ShowLauncher`。事件通道已最优；可感知延迟集中在 `ShowLauncher` 首帧前的同步块：`SyncDwmBorderColor` 全窗口屏幕采样（GDI GetDC+StretchBlt）、`ForceForeground` 里同步 `Thread.Sleep(10)×3`、`QueryBox.Text=""` 触发的防抖查询与显式查询重复两发。
- **已确认不需要动**：`--hidden` 后台预构造窗口策略（保留）；`ToggleWatcher` 阻塞等待；`OnFavRendering`（非动画期提前 return）；隐藏改"离屏不 Hide"（平台税，不划算）。

## 任务清单

### [x] ① UI 发布开 PublishReadyToRun
- [x] `ui/Spark.UI/Spark.UI.csproj` 加 `<PublishReadyToRun>true</PublishReadyToRun>`（仅 `dotnet publish` 生效，CI release.yml 的 publish 自动受益；开发 `dotnet build` 不受影响）
- [x] 收益：冷启动免 JIT，预计缩短 30-50%；代价安装包体积 +50-80MB

### [x] ② host 热键注册提前 + 索引后台化
- [x] `crates/index/src/lib.rs`：新增 `AppIndex::with_history_only()`（空内存索引 + 同步加载历史，默认页立即可用）
- [x] `crates/index/src/lib.rs`：新增 `swap_memory_with_reconcile()`（换入内存索引 + 补做 legacy .lnk 历史 id 重指向，对齐原同步启动语义）
- [x] `crates/host/src/index_watch.rs`：新增 `build_boot_index()`——后台线程全量扫描 Start Menu（复用 `REFRESHING` 闸防并发重建），完成后带 reconcile 换入
- [x] `crates/host/src/app.rs`：`bootstrap` 拆 `bootstrap_fast`（索引走 `with_history_only`）；`--query` 诊断模式保持同步全量 `bootstrap` 不变；顺带删除不再被引用的 `index_len`
- [x] `crates/host/src/main.rs`：守护路径改 `bootstrap_fast` + `build_boot_index`；删除 `sleep(30ms)`（UI 端 ConnectAsync 本有 1.5s×8 重试）；启动日志去掉误导性的 `indexed=0`
- **语义保持**：历史在 bootstrap 同步 load；reconcile 依赖新 memory，在后台换入时补做；插件扫描保持同步（本地 manifest 读取，毫秒级）
- **校验**：`cargo fmt` ✓；`cargo test -p spark-index -p spark-host` 46 通过 ✓

### [x] ③ ShowLauncher 瘦身（ui/Spark.UI/MainWindow.xaml.cs）
- [x] `SyncDwmBorderColor` 挪出 Show 之前的同步路径：SetWindowPos 已生效后 `Task.Run` 后台采样（GDI 屏幕读回不占首帧；边框色晚一两帧无感知；无 XAML 依赖，线程安全）
- [x] `ForceForeground` 删同步重试循环（`Thread.Sleep(10)×3` 最坏 30ms 卡 UI 线程）；重试职责移交已有的异步 `RetryFocusAsync`；`HandleToggle` 的 hide-animating 分支补挂 `RetryFocusAsync`
- [x] 唤起双重查询收敛：`Text=""` 触发的防抖查询与显式 `RefreshResultsAsync("")` 重复，显式那发后取消防抖 CTS
- **校验**：`dotnet build -c Debug` 0 警告 0 错误 ✓

### [x] ④ 启动耗时打点 + x:Load 据实决策
- [x] UI 打点（永久诊断，`App.Log("Startup", …)` 通道）：`InitializeComponent` / `ctor done` / `Root.Loaded` / `ShowLauncher body` 各阶段毫秒数
- [x] host 打点（tracing）：`bootstrap_fast done (boot_ms)` / `entering message loop (t_plus_ms)` / `app index ready (background, build_ms)`
- [x] **本机 Debug 实测**：InitializeComponent 1079ms；ctor 3986ms；Root.Loaded t+4071ms；ShowLauncher body 55ms→32ms（二次唤起，③ 瘦身后）
- [x] **决策：不做 SettingsPanel 的 x:Load**。理由：窗口 `--hidden` 预构造，ctor 成本不落在用户可感知路径（唤起走已构造好的窗口）；Release+R2R 后 XAML 解析预计减半以上；设置面板子树在 code-behind 有大量无条件引用，x:Load 需全部 null 守护，回归风险与收益不成比例。R2R（①）+ 后台索引（②）已覆盖冷启动大头

### [x] ⑤ 全量校验
- [x] `cargo fmt --check` 通过
- [x] `cargo test --workspace` 全部通过（140 个测试，0 失败）
- [x] `dotnet build -c Debug` 0 警告 0 错误；`dotnet publish -c Release`（R2R）成功
- [x] **R2R 发布版冒烟实测**：InitializeComponent 1079→232ms（-78%）；ctor 3986→1034ms（-74%）；Root.Loaded t+4071→t+1101ms；ShowLauncher body 56ms

### [x] ⑥ Code Auditor 审计闭环
- [x] code-reviewer 全量 diff 审计
- [x] 首轮 `[STATUS: NEEDS_FIX]`：🔴 采样/Show 竞态（Task.Run 后台采样可能在窗口可见后执行，采到窗口自身内容）→ 修复：恢复 Show 前同步采样（隐藏态不变量确定性成立）+ 专项打点（实测 15ms）+ 同步修正 SyncDwmBorderColor 文档注释；🟡 顺带更新 HostApp.index 字段注释
- [x] 回归 `[STATUS: PASSED]`：阻断项清零、无新副作用，批准交付

## 遗留非阻断项（审计记录，可另开任务）

- 🟡 `index_watch.rs` REFRESHING 闸非 panic 安全（后台线程若在持闸期 panic，闸被永久闩死、30s 热更新静默失效）——poll 路径既有模式，建议 catch_unwind 包裹（纯 std）另立任务。
- 🟡 锁内 reconcile+save（审计已核实成本有界：历史 ≤100 条、毫秒级、启动一次性）。
- 🟡 App.Log（File.AppendAllText）并发写可能丢诊断日志行（不崩溃；改锁会动共享基础设施）。

## 实施备注

- 工作区有会话前已存在的未提交 WIP（config.rs/ipc_server.rs/protocol.rs/FEATURES.md/MainWindow.xaml(.cs)/HostIpcClient.cs/LocalState.cs），在其上叠加，不回退。
- 热键注册时机变化后，"开机瞬间按热键"即响应（原先要等 bootstrap+UI 起完）；索引换入前搜索仅内置命令+历史兜底，窗口 <1s。