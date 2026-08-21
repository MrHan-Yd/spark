## 目标
1. 优化设置-插件页布局（卡片式现代化）
2. 通用页加"开发者模式"开关
3. 开发者模式开启 → 插件页每行显示"调试"按钮；关闭 → 不显示

## 改动文件

### 1. `ui/Spark.UI/Services/LocalState.cs` — 新增持久化字段
在 `AppUiState` 末尾加：
```csharp
/// <summary>开发者模式（通用设置）：开启后插件页显示调试按钮，插件窗口开放 DevTools。</summary>
public bool DeveloperMode { get; set; }
```

### 2. `ui/Spark.UI/ViewModels/PluginRowVm.cs` — 调试按钮可见性
- `using Spark.UI.Services;`
- 新增属性，构造时根据全局开关计算（列表在开关变化后整体重建，无需 INotify）：
```csharp
public Visibility DebugVisibility =>
    LocalState.Ui.DeveloperMode ? Visibility.Visible : Visibility.Collapsed;
```

### 3. `ui/Spark.UI/MainWindow.xaml` — XAML 改版

**3a. 通用页（PaneGeneral）末尾，悬浮球行之后加一行：**
"开发者模式" / "开启后插件页显示调试按钮，可用 DevTools 调试插件" + `DevModeSwitch`（SwitchStyle），Checked/Unchecked 接 `OnToggleDevMode`。

**3b. 插件页（PanePlugins）改版为卡片式：**

- **顶部工具栏卡片**（替换原"工具条 + 插件目录"两条 PaneRowBorder）：一张 `ChipBgBrush` 底 + `GlassBorderBrush` 描边 + CornerRadius 12 的卡片，内含：
  - 第一行：三个带图标的按钮 —— `+ 安装本地插件`（E710）、`📁 加载开发目录`（E8DA）、`🔄 刷新`（E72C），保留原 Click 处理器
  - 第二行：插件目录路径（`PluginDirText`）+ `更换…` 按钮

- **状态文本 `PluginStatus`、空态 `PluginEmpty` 保留不变。**

- **插件列表项改成卡片**：`ListViewItem` 容器 Margin 改为 `0,0,0,10`、Padding `0`、CornerRadius 12；DataTemplate 内层用 `Border`（`ChipBgBrush` + `GlassBorderBrush` + BorderThickness 1 + CornerRadius 12 + Padding 12,10）包住原 Grid。卡片内布局：
  - 列0：图标（沿用）
  - 列1：第一行 名称 + 版本 + 开发徽标；第二行 描述；第三行 关键字 chips；第四行 权限复选框（沿用各 ItemsControl）
  - 列2：启用 ToggleSwitch；下方一行水平 `调试`（Visibility=`{DebugVisibility}`, Click=OnDebugPlugin）+ `卸载`（沿用）两个小按钮

### 4. `ui/Spark.UI/MainWindow.xaml.cs` — 逻辑接线

- `SyncSettingsUi` 增 `DevModeSwitch.IsChecked = LocalState.Ui.DeveloperMode;`
- 新增 `OnToggleDevMode`：写 `LocalState.Ui.DeveloperMode` + `SaveUi`；非 syncing 时若已有插件行则调 `LoadPluginsAsync()` 重建（让调试按钮显隐刷新）
- 新增 `OnDebugPlugin`：取 row → `_host.PluginOpenAsync(row.Id, "", "")` → `PluginWindowHost.OpenOrFocus(..., devMode: true)` → 状态条提示；失败走 `SetPluginStatus` + `App.Log`
- 修改现有正常打开流程（约 4198 行）：`var devMode = LocalState.Ui.DeveloperMode || await IsDevPluginAsync(pluginId);` —— 开发者模式开时，所有插件窗口都开放 DevTools，语义一致

## 不改动
- host/Rust 侧不动（devMode 纯 UI 侧决定，PluginWindow 已支持）
- 现有 dev 插件自动开 DevTools 行为保留
- 其它设置页不动

## 验证
- `cargo fmt` / `cargo test --workspace`（本次无 Rust 改动，仍按规约跑一遍确认不回归）
- 编译 UI：`dotnet build`（ui/Spark.UI）确认 XAML/code-behind 绑定无缺失
- 按 AGENTS.md 流程：实现后调用 Code Auditor 子智能体审计，按 PASSED/NEEDS_FIX 闭环后再交付