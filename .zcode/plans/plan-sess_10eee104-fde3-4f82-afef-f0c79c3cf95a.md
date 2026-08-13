## 内置系统命令（学习 utools 做法）

### 背景
utools 的内置命令属于核心而非插件：静态命令表 + 别名/拼音缩写搜索 + 回车执行系统操作；不可逆操作（关机/重启/注销/清空回收站）会弹确认框；信息类命令（内网IP）回车复制结果。本项目 `Source::Builtin` 已存在但未使用，UI 已预留 builtin 的"系统"样式，无需适配。

### 实现步骤

**1. `crates/index/src/builtin.rs`（新建，纯数据+匹配，无 Win32）**
- `BuiltinSpec { id, title, subtitle, aliases: &[&str], confirm: Option<&'static str> }`
- 静态命令表（10 条）：
  - 锁屏（别名：锁定/lock/sd）、关机（poweroff/gj）、重启（restart/cq）、注销（logout/zx）、睡眠（sleep/sm）、清空回收站（回收站/recycle/qkhsz）、截图（屏幕截图/截屏/screenshot/jt）、设置（系统设置/sz）、文件资源管理（资源管理器/explorer）、内网IP（局域网IP/ip）
  - 其中关机/重启/注销/清空回收站带 `confirm` 文案（如"确认关机？"）
- `candidates(q) -> Vec<Candidate>`：title 包含或别名前缀匹配，打分风格对齐 `memory.rs`（精确+0.35/前缀+0.25/包含+0.12/别名+0.08），`Source::Builtin`，id 形如 `builtin.lock`
- `find(id) -> Option<&BuiltinSpec>`（invoke 路由用）
- 单元测试：中文/别名/拼音缩写匹配、空查询不返回、id 唯一

**2. `crates/index/src/lib.rs`**：导出 builtin 模块；`search_with_history` 非空查询分支合并 builtin 候选后统一 `rank_candidates`（默认页仍只显示历史，utools 同）

**3. `crates/ipc/src/protocol.rs`**：`InvokeResult` 新增 `Confirm { message: String }` 变体（serde tag `"confirm"`）

**4. `crates/host/src/builtins.rs`（新建，Win32 执行层）**
- `execute(id) -> Result<BuiltinOutcome>`，`BuiltinOutcome::{Close(msg), CopyText(text)}`
- 实现：
  - 锁屏 `LockWorkStation()`
  - 关机/重启/注销：启用 SeShutdownPrivilege（OpenProcessToken/LookupPrivilegeValueW/AdjustTokenPrivileges）+ `ExitWindowsEx`
  - 睡眠 `SetSuspendState(FALSE, TRUE, FALSE)`
  - 清空回收站 `SHEmptyRecycleBinW`（无进度条）
  - 设置 `ShellExecuteW("ms-settings:")`；文件资源管理 `explorer.exe`
  - 截图：优先 `ms-screenclip:`，失败回退 `snippingtool.exe`
  - 内网IP：`GetAdaptersAddresses` 取首个非回环/非链路本地的 IPv4 → `CopyText`

**5. `crates/host/src/app.rs`**：`invoke()` 先查 `builtin::find(item_id)`：
- 命中且 `confirm` 有值且 action=="open" → 返回 `InvokeResult::Confirm`（不执行）
- 否则 → `builtins::execute` → Close/CopyText；失败 → ShowError

**6. `crates/host/Cargo.toml`**：windows features 增加 `Win32_System_Shutdown`、`Win32_System_Power`、`Win32_NetworkManagement_IpHelper`、`Win32_Networking_WinSock`

**7. UI `ui/Spark.UI/MainWindow.xaml.cs`**：`InvokeActionAsync` 按 result type 分发：
- `confirm` → ContentDialog 弹窗（内容=message，主按钮"确认执行"，关闭按钮"取消"）；确认 → 以 `action_id="confirm"` 重新 invoke；取消 → Footer"已取消"
- `copy_text` → `Windows.ApplicationModel.DataTransfer.Clipboard` 复制 + Footer"已复制：xxx"
- `keep` → 仅 Footer 显示消息，不隐藏
- `show_error` → 现有逻辑

**8. 验证**：`cargo test --workspace` + `cargo fmt`；如环境可用再 `dotnet build` 验证 UI 编译