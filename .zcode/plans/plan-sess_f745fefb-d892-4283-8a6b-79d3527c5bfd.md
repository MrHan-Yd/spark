## 目标
uTools 式唤起：热键（或托盘/`ui.show`）唤醒时，窗口弹出在**鼠标当前所在显示器**上（水平居中、垂直 1/6 处，沿用现有首显公式），不再固定主屏，也不再记住拖拽位置。

## 改动范围
只改 1 个文件：`ui/Spark.UI/MainWindow.xaml.cs`。Rust host 零改动（热键注册、SetEvent 通道都不动——热键逻辑仍在 crates，UI 只负责窗口摆放，符合 AGENTS.md 分层）。

## 具体修改

### 1. P/Invoke 区（`MainWindow.xaml.cs` 底部，~2914 行起）
新增（user32，与现有声明风格一致）：
- `GetCursorPos(out POINT)` — 读鼠标坐标（PerMonitorV2 下为物理像素，与 AppWindow.Move 坐标系一致）
- `MonitorFromPoint(POINT, uint)` — 取鼠标所在显示器句柄
- `GetMonitorInfo(IntPtr, ref MONITORINFO)` — 取该屏工作区 `rcWork`
- `POINT { int X; int Y; }`、`MONITORINFO { int cbSize; RECT rcMonitor; RECT rcWork; uint dwFlags; }`（cbSize 用 `Marshal.SizeOf` 初始化）
- 常量 `MONITOR_DEFAULTTONEAREST = 0x00000002`

删除不再使用的：`EnumMonitorsProc` 委托 + `EnumDisplayMonitors` P/Invoke（仅被 SavedWindowPos 使用）。

### 2. 重写 `PlaceWindow(int w, int h)`（现 ~786 行）
- Resize + Move 到新 `CursorPlacement(w, h)` 的结果
- 删除 `_everPlaced` 字段与"已显示过不重排"分支——每次唤起都重新跟随鼠标屏
- 删除 `SavedWindowPos` 方法（不再读取 WindowX/WindowY；`HideNow` 里的位置落盘保留，无害，留给将来可能的设置项）

### 3. 新增 `CursorPlacement(int w, int h) -> PointInt32`
1. `GetCursorPos` → `MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)` → `GetMonitorInfo` 取 `rcWork`
2. 成功 → 居中：`work.X + (work.Width - w) / 2`，`work.Y + Math.Max(80, work.Height / 6)`（与现有首显公式一致）
3. Win32 任一失败 → 兜底回退旧行为：`DisplayArea.GetFromWindowId(_appWindow.Id, DisplayAreaFallback.Primary)` 主屏居中

### 4. `ShowLauncher`（现 ~888 行）
不改顺序：`SyncDwmBorderColor`（隐藏时采样）→ Show → `PlaceWindow`。因为每次 Show 都会重新定位，唤起即跟随鼠标屏。已知极小视觉边界：首次显示时 1px DWM 边框色在旧位置采样、随后移动到鼠标屏，下次唤起即自愈；不为此重排 Show 顺序（隐藏态 Move 是否生效不确定，避免引入风险）。

## 验证
- `dotnet build` Spark.UI 编译通过（语法/引用检查）
- `cargo test --workspace` 确认 Rust 侧无回归
- 多屏真机效果需你在双屏机器上手动验证（热键唤起 → 鼠标在副屏时窗口出现在副屏）