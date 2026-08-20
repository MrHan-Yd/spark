# 插件窗口 Spark 风格化 + 图标加载降级

## 目标
1. 插件弹窗从"系统默认标题栏"改为 Spark 风格自绘标题栏（图标 + 插件名 + 关闭按钮）
2. 窗口图标优先用开发者 `plugin.json` 配置的 icon；读不到降级用 Spark 项目自己的 `Assets/spark.png`
3. 附带修复设置页插件图标不显示的 bug（host 返回相对路径，UI 期望绝对路径）

## 改动文件

### 1. Rust host — `crates/plugin-manager/src/lib.rs`

**`PluginOpenInfo` 结构体加两个字段**（供 UI 渲染标题栏）：
```rust
pub struct PluginOpenInfo {
    pub id: String,
    pub name: String,              // 新增：插件显示名
    pub main_abs: String,
    pub window: PluginWindow,
    pub permissions: Vec<String>,
    pub granted: Vec<String>,
    pub preload_abs: Option<String>,
    pub root: String,
    pub icon_abs: Option<String>,  // 新增：icon 绝对路径（文件存在时），否则 None
}
```

**`open()` 填充新字段**：
- `name = p.manifest.name.clone()`
- `icon_abs`：`p.manifest.icon.as_ref().map(|ic| p.root.join(ic)).filter(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())`——清单声明 icon 且文件存在才返回绝对路径，否则 None（让 UI 走降级）

**附带修复 `list()` 的 icon**（同一文件）：
- `icon: p.manifest.icon.clone()` → 改为绝对路径同款逻辑：`manifest.icon` 拼 `root`、文件存在才返回，否则 None
- 这样 `PluginRowVm`（当绝对路径用，`File.Exists(info.Icon)`）才能真正显示设置页图标

> host 侧不负责"降级到 spark.png"——降级是 UI 概念，host 只给"插件自己 icon 的绝对路径或 null"。

### 2. C# DTO — `ui/Spark.UI/Models/PluginDto.cs`

`PluginOpenInfoDto` 加两个字段：
```csharp
[JsonPropertyName("name")]
public string Name { get; set; } = "";

[JsonPropertyName("icon_abs")]
public string? IconAbs { get; set; }
```

### 3. PluginWindow.xaml — 自绘 Spark 风格标题栏

XAML 从单一 Grid+WebView2 改为行布局：
```
Root (Background=ApplicationPageBackgroundThemeBrush)
 ├─ Row0: TitleBar Grid (Height=36)
 │   ├─ Image (图标 18×18, margin 12,4)
 │   ├─ TextBlock (插件名, 绑 _info.Name)
 │   └─ Button "×" (关闭, 右侧, hover 红 #FF453A)
 └─ Row1: WebView2 (x:Name=Web)
```
- 标题栏画刷用 `{ThemeResource}` 跟随系统深/浅色（背景同 `ApplicationPageBackgroundThemeBrush`，文字 `SystemControlForegroundBaseHighBrush`，底边一条 `SystemControlForegroundBaseLowBrush` 分隔线）
- 关闭按钮 hover 背景固定 `#FF453A`（Spark danger 色，深浅色都辨识）
- `frame:false`（插件自绘语义）时 TitleBar 区域 `Visibility=Collapsed`，保留原规范语义
- Root.Resources 内联 2-3 个画刷（关闭按钮 hover/正常态），不抽到 App.xaml（影响面最小）

### 4. PluginWindow.xaml.cs — 标题栏交互 + 图标降级

**标题栏拖拽**（标准 WinUI3 方式，比 MainWindow 的手动 caption region 简单）：
```csharp
ExtendsContentIntoTitleBar = true;
SetTitleBar(TitleBar);   // TitleBar 为 XAML 中 x:Name 的标题栏 Grid
```
- frame:false 时不调 SetTitleBar（无宿主标题栏）
- 关闭按钮 Click → Close()

**ApplyWindowSpec 调整**：
- frame=true：`SetBorderAndTitleBar(true, false)`（保留系统边框、隐藏系统标题栏），显示自绘标题栏
- frame=false：保持现状 `SetBorderAndTitleBar(true, false)` + 隐藏自绘标题栏
- 标题用 `_info.Name`（取代当前的 `_info.Id`）

**图标加载降级** `LoadIcon()`：
```
1. _info.IconAbs 非空 && File.Exists(IconAbs) → new BitmapImage(new Uri(IconAbs))
2. 否则降级 → new BitmapImage(new Uri(Path.Combine(AppContext.BaseDirectory, "Assets", "spark.png")))
```
> 用文件路径而非 `ms-appx:///`——本项目是 unpackaged self-contained 应用，`ms-appx:///` 对任意资源不可靠（已确认 csproj：`WindowsPackageType=None` + Assets 是 `Content` CopyToOutput）。

**顺便设窗口最小尺寸**（小修复，MinWidth/MinHeight 现在只在 resize IPC 时 clamp，不挡用户拖拽）：
```csharp
_appWindow.MinSize = new SizeInt32((int)(w.MinWidth*scale), (int)(w.MinHeight*scale));
```

## 不做（明确范围）
- 不做亚克力真玻璃标题栏（需独立 AcrylicSystemBackdrop，复杂度不值；纯色已满足"风格相同"。可作为后续增强）
- 不接实时主题切换（插件窗口用 ThemeResource 跟随系统主题，用户在 Spark 内切换主题时已开窗口不实时变——临时窗口可接受）
- 不改 host `PluginOpenParams` 协议（只加返回字段，不破坏请求）

## 验证
- `cargo build -p spark-host` + `cargo test --workspace`（host 改动）
- `dotnet build ui/Spark.UI/Spark.UI.csproj`（UI 改动）
- `cargo fmt`（Rust 改动）
- 手测：用 hello 插件（无 icon 配置）触发 → 标题栏显示 spark.png 降级图标 + "Hello" 名；给 hello 加 `icon.png` 字段并放图 → 显示插件自己的图标
- 按 AGENTS.md：完成后跑 Code Auditor 子智能体审计

## Review 闭环
代码完成 + 编译通过后，调用 Code Auditor 审计（架构合规/正确性/工程规范），按返回结果修复直至 PASSED 再交付。