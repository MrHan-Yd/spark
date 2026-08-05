## 目标
把 WinUI 弹出主窗（MainWindow：搜索模式 + 设置模式）按 ui-prototype 中间的主弹窗逐项对齐。只改 `ui/Spark.UI/`，不动 Rust。

## 改动文件
1. `ui/Spark.UI/MainWindow.xaml` — 主要重排
2. `ui/Spark.UI/MainWindow.xaml.cs` — 逻辑接线
3. `ui/Spark.UI/App.xaml` —（可选）全局资源，不放则并入 MainWindow.Resources

## 具体对齐项

**1. 主题资源化（仿原型 CSS 变量）**
- 把 XAML 里硬编码颜色收成页面级 Brush 资源：GlassBg、GlassBorder(#24FFFFFF)、GlassHighlight(顶部高光渐变)、Text(#EBEBEB)/TextSecondary(#8CFFFFFF)/TextTertiary(#61FFFFFF)、Accent(#0A84FF)、RowHover(#14FFFFFF)、RowActive(#470A84FF)、Divider(#1AFFFFFF)、ChipBg、FavBg、FooterBg。
- 颜色值直接取自原型 styles.css（rgba → ARGB）。
- 支持主题切换：深色玻璃（默认）/ 浅色玻璃 / 跟随系统 —— 代码里按 `AppUiState.Theme` 改 Brush.Color，UI 即时更新。

**2. 玻璃效果**
- `SetupChrome()` 里 try/catch 尝试 `SystemBackdrop = new DesktopAcrylicBackdrop()`（WinAppSDK 1.6），失败则保留现有稳定深色 fallback；构建后本地启动验证，若仍闪退则只保留 fallback。
- 顶部高光渐变叠加层（对应 `.launcher-glass::before`）；保留 DWM 圆角。

**3. 搜索行**
- 用 Path 描边图标替换 Segoe MDL2 填充图标，对齐原型 SVG：放大镜、列表三横线、四宫格、齿轮。
- 保留分段式视图切换（chip 底 + 边框 + 选中项 accent-soft）、设置 icon-tool 按钮、右侧「N 项」meta。

**4. 结果列表 / 平铺**
- 在资源里覆盖 ListViewItem / GridViewItem 的 PointerOver/Selected 系列画刷 → hover 白 8%、选中 accent-soft 圆角蓝（对齐 `.result-item.active`）。
- 列表项：icon 36/圆角9、标题 14 Medium、副标题 12、来源 11（已基本一致，微调）。
- 平铺：48 icon/圆角12、选中加 accent 描边环（对齐 `.results-grid .result-item.active`）。

**5. 收藏坞（差距最大的部分之一）**
- 按原型重建：折叠 chevron + 星标 + 「收藏」+ 数量 `(n)`；分组 tab（全部/工作/开发/日常，读 LocalState.Fav.Groups）+「+」新建分组（ContentDialog 输入，写入 LocalState）。
- 收藏项 72px 卡片：icon 36/圆角10 + 10px 名称，横向滚动，按 activeGroup 过滤，点击执行并隐藏。
- 折叠/展开、activeGroup 持久化到 LocalState；搜索时变淡（现有逻辑保留）。

**6. 底栏**
- chips 文案对齐原型：「Enter 打开 / Tab 动作 / Ctrl+, 设置」；右侧保留现有「Host · 极速 / 演示 · 本地 / 未找到相关结果」逻辑。

**7. 设置页（差距最大的部分之二）**
- 按原型重建：顶栏（← 返回 chip 按钮 + 居中「设置」标题）+ 左侧导航（通用/热键/外观/插件）+ 右侧 pane。
- 通用：开机启动 / 失焦时隐藏 / 执行后隐藏（ToggleSwitch 或自定义 switch）+ 托盘提示 + Host 连接状态。
- 热键：Alt+Space / Ctrl+Space 预设按钮 + 提示。
- 外观：主题 select、默认视图 select、窗口宽度 slider（560–840，实时 Resize 生效）、减少动画 checkbox。
- 插件：Echo / JSON 格式化两行（switch）+「安装本地插件…」按钮（占位反馈）。
- 全部读写 `LocalState.AppUiState` 并 SaveUi；启动时应用 Theme/DefaultView/WindowWidth/ReduceMotion；减少动画时禁用弹出动画。

**8. 弹出动画**
- ShowLauncher：0.28s pop-in（opacity + scale 0.96 + translateY 6，对应 `pop-in` keyframes）；HideLauncher：0.16s pop-out。ReduceMotion 时跳过。

## 验证
1. `dotnet build ui\Spark.UI\Spark.UI.csproj -c Debug`（无 Rust 改动，不需要 cargo test）
2. 从 win-x64 目录启动 spark-ui.exe → 用第二个实例触发 toggle 显示窗口 → PowerShell 截图
3. 用视觉工具对照原型（直接分析截图，必要时开 ui-prototype/index.html 对比），迭代直到差距明显缩小
4. 确认无 acrylic 闪退、设置项读写正常后收尾