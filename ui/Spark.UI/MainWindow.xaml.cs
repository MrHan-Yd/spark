using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Diagnostics;
using System.Net;
using System.Net.Http.Headers;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using XamlPath = Microsoft.UI.Xaml.Shapes.Path;
using Microsoft.UI.Xaml.Media.Animation;
using Microsoft.Win32;
using Spark.UI.Models;
using Spark.UI.Services;
using Spark.UI.ViewModels;
using Windows.ApplicationModel.DataTransfer;
using Windows.Foundation;
using Windows.Graphics;
using Windows.System;
using Windows.UI;
using Windows.UI.Input.Preview.Injection;
using WinRT;
using WinRT.Interop;

namespace Spark.UI;

public sealed partial class MainWindow : Window
{
    private readonly BulkObservableCollection<CandidateDto> _items = new();
    private readonly HostIpcClient _host = new();
    private AppWindow? _appWindow;
    private TrayService? _tray;
    /// <summary>桌面悬浮球（uTools 式贴边悬浮球）；「通用设置 → 悬浮球」开关控制，null = 未启用。</summary>
    private FloatingBallWindow? _ball;
    private ToggleWatcher? _toggleWatcher;
    private ExitWatcher? _exitWatcher;
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _showFallback;
    /// <summary>--hidden：host 后台拉起（boot/开机自启/安装后启动），不预热不弹出，
    /// 只连 IPC + 托盘常驻，等快捷键/托盘唤起。直接双击 Spark.exe 无此参数，正常弹出。</summary>
    private readonly bool _startHidden;
    private int _active;
    /// <summary>收藏卡片选中索引（-1 = 未选中，焦点在结果区）；方向键可在结果区 ↔ 收藏区之间移动。</summary>
    private int _favActive = -1;
    private bool _loaded;
    private bool _visible;
    private bool _gridView;
    private IntPtr _hwnd;
    private int _queryGen;
    /// <summary>搜索防抖：快速连续输入只发最后一轮查询（配合 _queryGen 丢弃过期结果）。
    /// 80ms：体感"跟手"与合并按键的平衡点；首字符查询不受防抖（见 _lastScheduledQuery）。</summary>
    private const int QueryDebounceMs = 80;
    /// <summary>上一次调度查询的文本（UI 线程字段）：为空而本次非空 = 首字符，立即查询不防抖，
    /// 敲第一个字母结果就走；组词中 ScheduleRefresh 早退不更新，上屏补查按首字符语义立即发。</summary>
    private string _lastScheduledQuery = "";
    /// <summary>窗口固定宽度（设置已移除宽度滑杆，不再读取 LocalState.Ui.WindowWidth）。</summary>
    private const int LauncherWidth = 800;
    private CancellationTokenSource? _debounceCts;
    /// <summary>自定义亚克力 backdrop（可调 tint，macOS vibrancy 参数）；系统禁用透明效果时不用。</summary>
    private readonly AcrylicSystemBackdrop _acrylicBackdrop = new();
    /// <summary>Acrylic 不可用（系统禁用透明效果/环境不支持）时的纯色背景，颜色随主题。</summary>
    private readonly SolidColorBrush _windowBg = new();
    /// <summary>前台变化钩子句柄（失焦隐藏用，不依赖 Activated 事件）。</summary>
    private IntPtr _fgHook;
    /// <summary>钩子回调委托必须持有引用，否则被 GC 回收后回调悬空导致崩溃。</summary>
    private readonly WinEventDelegate _fgHookProc;
    /// <summary>显示后短时忽略失焦，避免 ForceForeground / 热键抢焦导致闪一下就关。</summary>
    private long _ignoreDeactivateUntilTicks;
    /// <summary>合并 event + pipe 双通道重复 toggle。</summary>
    private long _lastToggleTicks;

    /// <summary>隐藏 pop-out 动画播放中（_visible 尚未复位，此时收到 toggle 应取消关闭）。</summary>
    private bool _hideAnimating;
    /// <summary>隐藏代数：显示/取消隐藏时 +1，让已排队的 _animOut.Completed→HideNow 失配跳过（防止刚显示的窗口又被延迟的 HideNow 关掉）。</summary>
    private int _hideGen;
    /// <summary>本次隐藏动画开始时的代数。</summary>
    private int _animHideGen;
    /// <summary>IME 组词中：期间箭头键交给输入法（不拦截）。</summary>
    private bool _composing;
    /// <summary>上次 ImeKick 注入时间（TickCount64）：300ms 内不重复注入——
    /// 两次快速连点会被 XAML 识别为双击选词，反而破坏输入框内容。</summary>
    private long _lastImeKickTicks;
    /// <summary>当前打开的动作菜单（右键/Tab 共用）；打开期间根上箭头键交给菜单，不移动选中项。</summary>
    private MenuFlyout? _itemMenu;

    // 弹出动画（对齐原型 pop-in / pop-out）
    private readonly CompositeTransform _pop = new();
    private Storyboard _animIn = new();
    private Storyboard _animOut = new();
    /// <summary>同步设置控件时避免触发保存/换主题副作用。</summary>
    private bool _syncing;
    private HostConfigUpdate? _pendingHostConfig;
    /// <summary>列表/平铺切换动画；_viewAnimGen 让旧动画的 Completed 回调失配（快速连续切换不误折叠面板）。</summary>
    private Storyboard? _viewAnim;
    private int _viewAnimGen;
    /// <summary>主界面 ↔ 设置页切换动画（对齐原型 mode-leave-left/right + mode-enter-from-left/right）：
    /// 打开 = 主页左滑淡出 + 设置页从右滑入；关闭 = 反向。交叉过渡（两个面板同时动）。
    /// _modeAnimating 防动画中重复触发（220ms 内连点/连按 Esc 忽略）。</summary>
    private Storyboard _modeOutMain = new(), _modeInMain = new(), _modeOutSet = new(), _modeInSet = new();
    private bool _modeAnimating;
    /// <summary>收藏坞抽屉动画：布局高度在每渲染帧（CompositionTarget.Rendering）手动驱动——
    /// WinUI 3 的 Storyboard DoubleAnimation 对 Height 这类布局属性不渲染中间帧（实测零中间态），
    /// 普通属性赋值才能保证每帧触发布局重排；用渲染帧同步驱动（而非 DispatcherTimer）让动画值
    /// 与渲染帧对齐、每帧只布局一次，避免帧错位造成的卡顿。_favAnimGen 让被中断的旧动画落定失配。</summary>
    private bool _favTweening;
    private int _favAnimGen;
    private bool _favCollapsing;
    private double _favH0, _favH1, _favO0, _favG0, _favA0, _favS0;
    /// <summary>分组列起始宽度（随动画同步收放，避免落定时列宽瞬间跳变造成尾部卡顿）。</summary>
    private double _favGroupsW0;
    /// <summary>仅主体高度过渡（收藏内容空↔有项）：不动抽屉的间距/分组列宽/加号按钮收放。</summary>
    private bool _favHeightOnly;
    private long _favTweenStart;
    private int _favTweenMs;
    /// <summary>分组切换过渡：旧内容淡出 → 重建 → 容器淡入上移 + 卡片逐项 pop（对齐原型 is-leaving/is-entering）。
    /// 独立于 _favTweening 的状态机，两段动画可同时跑（高度过渡只管 Height，这里只管透明度/位移）。</summary>
    private bool _favSwitching;
    /// <summary>当前切换阶段：true = 旧内容淡出，false = 新内容入场。</summary>
    private bool _favSwitchOut;
    /// <summary>阶段起点容器透明度/位移（从中途续动，快速连续切组不跳变）。</summary>
    private double _favSwitchO0, _favSwitchY0;
    private long _favSwitchPhaseStart;
    private int _favSwitchPhaseMs;
    /// <summary>入场阶段要逐项 pop 的卡片及其变换（重建后捕获，结束时清空还原）。</summary>
    private readonly List<(Button Btn, TranslateTransform Shift, ScaleTransform Scale)> _favSwitchItems = new();

    public MainWindow()
    {
        _startHidden = Environment.GetCommandLineArgs().Any(a => a == "--hidden");
        // 启动诊断：ctor 各阶段耗时（对齐 ui-crash.log 现有打点风格，量化冷启动）
        var _ctorSw = System.Diagnostics.Stopwatch.StartNew();
        long _xamlMs;

        // WinUI 3 窗口构造即显示：先隐藏，等 XAML 内容渲染完成（Content.Loaded）再显示，
        // 消除启动时"空白窗口框弹一下就消失"的体感问题。
        this.AppWindow.Hide();

        try { InitializeComponent(); }
        catch (Exception ex) { App.Log("InitializeComponent", ex); throw; }
        _xamlMs = _ctorSw.ElapsedMilliseconds;
        App.Log("Startup", $"InitializeComponent {_xamlMs}ms");

        // pane 切换过渡用的位移变换映射（XAML 字段须在 InitializeComponent 之后才能引用）
        _paneShifts[PaneGeneral] = PaneGeneralShift;
        _paneShifts[PaneAppearance] = PaneAppearanceShift;
        _paneShifts[PaneBuiltins] = PaneBuiltinsShift;
        _paneShifts[PanePlugins] = PanePluginsShift;
        _paneShifts[PaneAbout] = PaneAboutShift;

        // 市场筛选初选"全部"：与 ThemeCombo 同款代码后置范式（XAML 解析期设 IsSelected 会
        // 同步触发 SelectionChanged，早于列表元素构造导致空引用）
        MarketFilterCombo.SelectedIndex = 0;

        // 关于页：版本号（csproj Version，与 Cargo 工作区一致）与初始更新状态
        AboutVersionText.Text = $"版本 {AppVersionText}";
        AboutUpdateStatus.Text = $"当前版本 {AppVersionText}";

        ExtendsContentIntoTitleBar = true;
        ResultList.ItemsSource = _items;
        ResultGrid.ItemsSource = _items;
        LocalState.Load();
        ApplyDevModeGate(LocalState.Ui.DeveloperMode);
        // 旧版 Run 项指向 Spark.exe（UI）→ 自动迁移到 spark-host.exe，修复开机热键失效
        MigrateStartupEntry();

        // 箭头键在根上全局接管（handledEventsToo=true：输入框有文本时会先消费箭头键并把事件标为已处理，
        // 普通 KeyDown 收不到；这里在冒泡终点仍能收到），统一只控制下面选中项
        Root.AddHandler(UIElement.KeyDownEvent, new KeyEventHandler(OnRootKeyDown), true);
        // 右键菜单：列表/平铺共用（handledEventsToo 保证行内元素命中也能收到）
        ResultList.AddHandler(UIElement.RightTappedEvent, new RightTappedEventHandler(OnResultRightTapped), true);
        ResultGrid.AddHandler(UIElement.RightTappedEvent, new RightTappedEventHandler(OnResultRightTapped), true);
        // IME 组词期间不拦截箭头（组词光标移动由输入法管），否则会破坏中文输入
        // 日志只在状态真正翻转时写一次：个别 TSF 会连发 Started/Ended，逐条同步落盘会拖慢输入
        QueryBox.TextCompositionStarted += (_, _) =>
        {
            if (_composing) return;
            _composing = true;
            App.Log("Ime", "composition started");
        };
        // 组词结束（上屏/取消）补一次查询：组词期间 TextChanged 也在触发，已被 ScheduleRefresh 挂起
        QueryBox.TextCompositionEnded += (_, _) =>
        {
            if (!_composing) return;
            _composing = false;
            App.Log("Ime", "composition ended");
            ScheduleRefresh(QueryBox.Text ?? "");
        };

        Root.RenderTransform = null;
        // 动画只作用于 ContentClip：背景是系统 Acrylic 真玻璃（DWM 模糊 + 半透明 tint），
        // 弹出动画期间窗口底层是模糊玻璃，不会露出黑色。
        ContentClip.RenderTransform = _pop;
        // 拖拽 caption 区域跟随窗口尺寸（宽度滑杆/布局变化）
        Root.SizeChanged += (_, _) => UpdateDragRegions();
        // 钩子回调委托持有引用防 GC（字段初始化器不能引用实例方法，放构造函数）
        _fgHookProc = OnForegroundChanged;        // 背景：系统 Acrylic 真玻璃（macOS vibrancy 同构），深浅主题共用；
        // 系统禁用透明效果时回退纯色面板（ApplyTheme 里赋 _windowBg），任何机器效果一致。

        ApplyTheme();
        SetView(LocalState.Ui.DefaultView == "grid");
        CompositionTarget.Rendering += OnFavRendering;
        BuildAnimations();

        try { SetupChrome(); } catch (Exception ex) { App.Log("SetupChrome", ex); }
        // WinUI 启动时会按 App 主题重设 DWM 深色属性，这里在 SetupChrome 之后再压一次，
        // 否则浅色系统下圆角外框会是白色（即"外面那层白色"）
        try { ApplyDwmDarkMode(); } catch (Exception ex) { App.Log("DwmDarkMode", ex); }

        _host.HostNotification += OnHostNotification;
        _host.Connected += OnHostConnected;

        // 热键主路径：Host SetEvent → 这里唤醒（不依赖 pipe 推送）
        _toggleWatcher = new ToggleWatcher(() =>
            DispatcherQueue.TryEnqueue(HandleToggle));

        // 托盘"退出"/host.exit → 整个应用退出（独立进程需要自己的退出信号）
        _exitWatcher = new ExitWatcher(() =>
            DispatcherQueue.TryEnqueue(OnHostExit));

        // 内容渲染完成后再显示窗口（配合构造开头 AppWindow.Hide 消除启动空壳框）。
        // 兜底：2 秒后仍未显示则强制显示，防止 Loaded 未触发的极端情况导致窗口永久隐藏。
        if (Content is FrameworkElement root)
        {
            if (_startHidden)
            {
                // 静默后台模式（host --hidden 拉起）：保持隐藏常驻，绝不弹出。
                // 首帧预热（Show→Hide）会闪一下窗口，违背"默默运行"；ShowLauncher
                // 自带焦点保护与防白圈处理，首次唤起即时渲染即可。这里不挂任何
                // 显示逻辑，窗口由快捷键/托盘（HandleToggle/ShowLauncher）唤起。
            }
            else
            {
                root.Loaded += (_, _) => ShowAfterReady();
                _showFallback = DispatcherQueue.CreateTimer();
                _showFallback.Interval = TimeSpan.FromSeconds(2);
                _showFallback.Tick += (_, _) => { _showFallback.Stop(); ShowAfterReady(); };
                _showFallback.Start();
            }
        }

        // 预热一帧再正式显示：窗口隐藏时合成器不提交帧，直接 Show 的瞬间是"空窗口"
        // （首帧未提交，加上 Acrylic backdrop 未连接，体感就是"空白框弹一下"）。
        // 先显示一帧触发合成器提交与 backdrop 连接，立即隐藏，再走 ShowLauncher 完整
        // 流程（动画/焦点/失焦保护），此时首帧即完整内容，无空白框。
        void ShowAfterReady()
        {
            if (this.AppWindow.IsVisible) return;
            this.AppWindow.Show();
            DispatcherQueue.TryEnqueue(() =>
            {
                this.AppWindow.Hide();
                DispatcherQueue.TryEnqueue(ShowLauncher);
            });
        }

        Activated += (_, e) =>
        {
            if (e.WindowActivationState == WindowActivationState.Deactivated)
            {
                // 刚显示时忽略失焦（抢前台过程会短暂 Deactivated）
                if (Environment.TickCount64 < _ignoreDeactivateUntilTicks)
                    return;
                // 失焦隐藏是固化默认行为（设置页不提供开关）
                if (_visible && SettingsPanel.Visibility != Visibility.Visible)
                {
                    HideLauncher();
                    App.Log("Focus", "deactivated -> hide");
                }
                else
                {
                    App.Log("Focus", $"deactivated skipped: visible={_visible}");
                }
                return;
            }
            QueryBox.Focus(FocusState.Keyboard);
            // 激活恢复：焦点往返强制 GotFocus 重触发，确保 TSF 重新挂接
            // （唤起/跨屏 Move/抢焦点后窗口激活链变化，输入法候选窗会丢，
            // 只有真实的 LostFocus→GotFocus 会让 TextBox 重新挂接文档管理器）
            ResultList.Focus(FocusState.Programmatic);
            QueryBox.Focus(FocusState.Keyboard);
            ResetIme();
        };

        Closed += async (_, _) =>
        {
            SystemBackdrop = null;   // 触发 AcrylicSystemBackdrop.OnTargetDisconnected 清理
            if (_fgHook != IntPtr.Zero)
            {
                try { UnhookWinEvent(_fgHook); } catch { /* ignore */ }
                _fgHook = IntPtr.Zero;
            }
            try { RemoveWindowSubclass(_hwnd, _noMaximizeProc, new UIntPtr(CaptionSubclassId)); } catch { /* ignore */ }
            _toggleWatcher?.Dispose();
            _toggleWatcher = null;
            _exitWatcher?.Dispose();
            _exitWatcher = null;
            HideBall();  // 悬浮球独立于主窗生命周期，随应用退出一起销毁
            await _host.DisposeAsync();
            HideLauncher();  // 正常关闭按钮 = 隐藏，不是真正关闭，保留托盘
        };

        Root.Loaded += async (_, _) =>
        {
            if (_loaded) return;
            _loaded = true;
            App.Log("Startup", $"Root.Loaded t+{_ctorSw.ElapsedMilliseconds}ms (XAML {_xamlMs}ms)");
            // 构造时窗口句柄可能未就绪（Acrylic 挂 target 需要），Loaded 后重放一次主题
            try { ApplyTheme(); } catch (Exception ex) { App.Log("Theme", ex); }
            try { await _host.EnsureConnectedAsync(); } catch (Exception ex) { App.Log("HostConnect", ex); }
            try { await SyncFromHostAsync(); } catch (Exception ex) { App.Log("HostConfig", ex); }
            try { SetupTray(); } catch (Exception ex) { App.Log("SetupTray", ex); }
            if (LocalState.Ui.FloatingBallEnabled) ShowBall();
            HideLauncher();
            await RefreshResultsAsync("");
            RenderFavorites();
            ApplyFavCollapse(!LocalState.Fav.Expanded, animate: false);
            _ = MaintainHostConnectionAsync();
        };

        App.Log("Startup", $"ctor done {_ctorSw.ElapsedMilliseconds}ms (XAML {_xamlMs}ms)");
    }

    // ==================== 主题 ====================

    private static bool SystemUsesDark()
    {
        try
        {
            using var k = Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            return k?.GetValue("AppsUseLightTheme") is int v && v == 0;
        }
        catch { return true; }
    }

    /// <summary>按 AppUiState.Theme 直接改画刷 Color（引用同一实例，UI 即时更新）。</summary>
    private void ApplyTheme()
    {
        bool dark = LocalState.Ui.Theme switch
        {
            "light" => false,
            "dark" => true,
            _ => SystemUsesDark(),
        };

        // key → ARGB；深/浅各一套，值对齐 styles.css 变量。
        // 浅色文字比原型略深：浅色真玻璃背景会透出桌面明暗，太淡的小字看不清
        var pal = new Dictionary<string, uint>
        {
            ["GlassBorderBrush"] = dark ? 0x24FFFFFFu : 0xA6FFFFFFu,
            ["TextPrimaryBrush"] = dark ? 0xFFEBEBEBu : 0xF5000000u,
            ["TextSecondaryBrush"] = dark ? 0x8CFFFFFFu : 0x99000000u,
            ["TextTertiaryBrush"] = dark ? 0x61FFFFFFu : 0x80000000u,
            ["AccentBrush"] = dark ? 0xFF0A84FFu : 0xFF007AFFu,
            ["AccentSoftBrush"] = dark ? 0x380A84FFu : 0x26007AFFu,
            ["RowHoverBrush"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["RowActiveBrush"] = dark ? 0x470A84FFu : 0x33007AFFu,
            ["RowActiveStrongBrush"] = dark ? 0x5C0A84FFu : 0x40007AFFu,
            ["DividerBrush"] = dark ? 0x1AFFFFFFu : 0x14000000u,
            ["ChipBgBrush"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["FavBgBrush"] = dark ? 0x24000000u : 0x0D000000u,
            ["FooterBgBrush"] = dark ? 0x1F000000u : 0x0F000000u,
            ["GridTileBgBrush"] = dark ? 0x08FFFFFFu : 0x0A000000u,
            ["GridTileSelBgBrush"] = dark ? 0x1EFFFFFFu : 0x16000000u,
            ["GridTileSelBorderBrush"] = dark ? 0x40FFFFFFu : 0x40000000u,
            ["SwitchTrackOffBrush"] = dark ? 0x52303038u : 0x33000000u,
            ["StarBrush"] = 0xFFFFD60Au,
            // 下拉弹出层：半透明玻璃（深色 75% #1C1C1E / 浅色 75% 白，透出壁纸色斑）
            ["ComboPopupBgBrush"] = dark ? 0xC01C1C1Eu : 0xC0FFFFFFu,
            // 模态卡片底（新建分组/删除确认）：深色近黑、浅色近白
            ["ModalCardBrush"] = dark ? 0xF21C1C1Eu : 0xF2FFFFFFu,
            // 列表 / 平铺 选中态
            ["ListViewItemBackgroundPointerOver"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["ListViewItemBackgroundPressed"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["ListViewItemBackgroundSelected"] = dark ? 0x470A84FFu : 0x33007AFFu,
            ["ListViewItemBackgroundSelectedPointerOver"] = dark ? 0x5C0A84FFu : 0x40007AFFu,
            ["ListViewItemBackgroundSelectedPressed"] = dark ? 0x5C0A84FFu : 0x40007AFFu,
            ["ListViewItemBorderBrushPointerOver"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["ListViewItemBorderBrushPressed"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["ListViewItemBorderBrushSelected"] = dark ? 0x470A84FFu : 0x33007AFFu,
            ["GridViewItemBackgroundPointerOver"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["GridViewItemBackgroundPressed"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["GridViewItemBackgroundSelected"] = dark ? 0x470A84FFu : 0x33007AFFu,
            ["GridViewItemBackgroundSelectedPointerOver"] = dark ? 0x5C0A84FFu : 0x40007AFFu,
            ["GridViewItemBackgroundSelectedPressed"] = dark ? 0x5C0A84FFu : 0x40007AFFu,
            ["GridViewItemBorderBrushPointerOver"] = dark ? 0x1AFFFFFFu : 0x14000000u,
            ["GridViewItemBorderBrushSelected"] = dark ? 0x590A84FFu : 0x40007AFFu,
            ["GridViewItemBorderBrushSelectedPointerOver"] = dark ? 0x590A84FFu : 0x40007AFFu,
        };
        foreach (var (key, c) in pal)
        {
            if (Root.Resources.TryGetValue(key, out var o) && o is SolidColorBrush b)
                b.Color = Color.FromArgb((byte)(c >> 24), (byte)(c >> 16), (byte)(c >> 8), (byte)c);
        }

        if (Root.Resources.TryGetValue("GlassHighlightBrush", out var g) && g is LinearGradientBrush hl)
            hl.GradientStops[0].Color = Color.FromArgb(dark ? (byte)0x14 : (byte)0x66, 0xFF, 0xFF, 0xFF);

        // 真玻璃（深浅通用，macOS vibrancy 同构）：自定义 SystemBackdrop（AcrylicSystemBackdrop）
        // 由框架渲染在 XAML 内容层之后——DWM 模糊窗口后面的内容 + 半透明 tint。
        // 系统禁用透明效果时回退纯色面板（_windowBg，颜色随主题）。
        _windowBg.Color = Color.FromArgb(0xFF,
            dark ? (byte)0x1C : (byte)0xF2,
            dark ? (byte)0x1C : (byte)0xF5,
            dark ? (byte)0x1E : (byte)0xFA);
        var useAcrylic = TransparencyEnabled();
        if (useAcrylic)
        {
            // 防重复 connect：SystemBackdrop 赋值相同实例可能触发 disconnect/connect 循环
            if (!ReferenceEquals(SystemBackdrop, _acrylicBackdrop))
                SystemBackdrop = _acrylicBackdrop;
            _acrylicBackdrop.ApplyTheme(dark);
        }
        else if (SystemBackdrop is not null)
        {
            SystemBackdrop = null;
        }
        Root.Background = useAcrylic ? null : _windowBg;

        // 内容/系统控件主题跟随玻璃主题（浅色系统下 TextBox 光标、滚动条、弹窗默认是浅色的）
        Root.RequestedTheme = dark ? ElementTheme.Dark : ElementTheme.Light;
        ApplyDwmDarkMode();
        _ball?.ApplyTheme(dark);
    }

    /// <summary>系统是否开启透明效果。关闭时 Acrylic 会静默回退成纯色，此时回退纯色背景。</summary>
    private static bool TransparencyEnabled()
    {
        try
        {
            using var k = Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            return k?.GetValue("EnableTransparency") is int v && v == 1;
        }
        catch { return true; }
    }

    /// <summary>DWM 圆角外框颜色跟随主题；WinUI 启动时按 App 主题重设过该属性，主题切换时再压一次。</summary>
    private void ApplyDwmDarkMode()
    {
        if (_hwnd == IntPtr.Zero) return;
        bool dark = LocalState.Ui.Theme switch
        {
            "light" => false,
            "dark" => true,
            _ => SystemUsesDark(),
        };
        const int DWMWA_USE_IMMERSIVE_DARK_MODE = 20;
        var v = dark ? 1 : 0;
        DwmSetWindowAttribute(_hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, ref v, sizeof(int));

        // 圆角外框颜色：系统浅色主题下 DWM 会用白色画外框（就是"外面那层白色"），
        // 深色属性只管标题栏，外框要用 DWMWA_BORDER_COLOR（34，Win11 22H2+）直接指定。
        // 这里只做静态兜底（玻璃感知色随背景浮动，静态值只能匹配一种背景），
        // 每次显示前由 SyncDwmBorderColor 采样背景做精确匹配。
        const int DWMWA_BORDER_COLOR = 34;
        var border = dark ? (int)0x004C4A4Au : (int)0x00F2F2F7u; // COLORREF 0x00BBGGRR，取玻璃边缘感知色
        DwmSetWindowAttribute(_hwnd, DWMWA_BORDER_COLOR, ref border, sizeof(int));
    }

    /// <summary>
    /// 把 DWM 圆角外框（1px，Win11 22H2+ 对圆角窗口强制绘制，无 API 可移除）设成
    /// "当前玻璃感知色"：玻璃 = tint 按 TintOpacity 叠背景模糊，感知色随背景在近黑↔浅灰
    /// 之间浮动，静态色必然在某种背景上突兀。显示前（窗口仍隐藏）采样窗口矩形区域的
    /// 屏幕均值，按 AcrylicSystemBackdrop 的 tint 参数算出感知色，任何背景下外框都与
    /// 玻璃融为一体。窗口可见时采样会把窗口自身算进去，所以只在隐藏时调用（ShowLauncher）。
    /// 采样必须同步发生在 Show 之前，不得后台化：后台线程的执行时机与 Show 竞态，
    /// 一旦在窗口可见后才采样，矩形里是窗口自身（玻璃+内容）而非桌面背景，边框色
    /// 会随结果列表内容/时序漂移（且对已 tint 过的玻璃像素二次叠 tint）。
    /// </summary>
    private void SyncDwmBorderColor()
    {
        if (_hwnd == IntPtr.Zero) return;
        try
        {
            if (!TransparencyEnabled()) return;   // 纯色回退时 ApplyDwmDarkMode 的静态色已匹配
            if (!GetWindowRect(_hwnd, out var r) || r.Right <= r.Left || r.Bottom <= r.Top)
                return;

            // StretchBlt 下采样到 24x18 的 32bpp DIB 再取均值，近似 DWM 模糊后的背景色
            const int SW = 24, SH = 18;
            var w = r.Right - r.Left;
            var h = r.Bottom - r.Top;
            var src = GetDC(IntPtr.Zero);
            if (src == IntPtr.Zero) return;
            long sumR = 0, sumG = 0, sumB = 0;
            try
            {
                var bmi = new BITMAPINFO();
                bmi.biSize = 40;                 // sizeof(BITMAPINFOHEADER)
                bmi.biWidth = SW;
                bmi.biHeight = -SH;              // 负值 = 自顶向下，读取顺序与屏幕一致
                bmi.biPlanes = 1;
                bmi.biBitCount = 32;
                var hbmp = CreateDIBSection(src, ref bmi, 0, out var bits, IntPtr.Zero, 0);
                if (hbmp == IntPtr.Zero) return;
                var mem = CreateCompatibleDC(src);
                if (mem == IntPtr.Zero) { DeleteObject(hbmp); return; }
                try
                {
                    var old = SelectObject(mem, hbmp);
                    try
                    {
                        // HALFTONE：缩小时按像素均值填充（否则是跳采样，噪声大）
                        SetStretchBltMode(mem, HALFTONE);
                        StretchBlt(mem, 0, 0, SW, SH, src, r.Left, r.Top, w, h, SRCCOPY);
                    }
                    finally { SelectObject(mem, old); }

                    var px = new byte[SW * SH * 4];
                    Marshal.Copy(bits, px, 0, px.Length);
                    for (var i = 0; i < px.Length; i += 4)
                    {
                        sumB += px[i];      // DIB 32bpp 是 BGRA
                        sumG += px[i + 1];
                        sumR += px[i + 2];
                    }
                }
                finally { DeleteDC(mem); DeleteObject(hbmp); }
            }
            finally { ReleaseDC(IntPtr.Zero, src); }

            var n = SW * SH;

            // 感知色 = avg * (1 - TintOpacity) + tint * TintOpacity（与 AcrylicSystemBackdrop 参数一致）
            bool dark = LocalState.Ui.Theme switch
            {
                "light" => false,
                "dark" => true,
                _ => SystemUsesDark(),
            };
            byte fr, fg, fb;
            if (dark)
            {
                fr = (byte)((sumR / n + 0x1C) / 2);
                fg = (byte)((sumG / n + 0x1C) / 2);
                fb = (byte)((sumB / n + 0x1E) / 2);
            }
            else
            {
                fr = (byte)((sumR / n * 55 + 0xF2 * 45) / 100);
                fg = (byte)((sumG / n * 55 + 0xF5 * 45) / 100);
                fb = (byte)((sumB / n * 55 + 0xFA * 45) / 100);
            }

            // COLORREF 0x00BBGGRR
            var border = (fb << 16) | (fg << 8) | fr;
            const int DWMWA_BORDER_COLOR = 34;
            DwmSetWindowAttribute(_hwnd, DWMWA_BORDER_COLOR, ref border, sizeof(int));
        }
        catch (Exception ex)
        {
            App.Log("BorderColor", ex);
        }
    }

    /// <summary>
    /// 彻底去掉非客户区边框：WinUI 的 SetBorderAndTitleBar(false,false) 会残留 WS_DLGFRAME/WS_SYSMENU，
    /// DWM 按系统浅色主题把这块 ~3px 画成白色（"外面那层白色"），客户区也相应内缩 3px。
    /// 清掉后客户区 = 窗口，白圈消失。
    /// </summary>
    private void MakeFrameless()
    {
        if (_hwnd == IntPtr.Zero) return;
        const int GWL_STYLE = -16;
        const int WS_DLGFRAME = 0x00400000;
        const int WS_SYSMENU = 0x00080000;
        var s = GetWindowLong(_hwnd, GWL_STYLE) & ~(WS_DLGFRAME | WS_SYSMENU);
        SetWindowLong(_hwnd, GWL_STYLE, s);
        // FRAMECHANGED 让样式立即生效，重算非客户区
        const uint SWP_FRAMECHANGED = 0x0020, SWP_NOMOVE = 0x0002, SWP_NOSIZE = 0x0001;
        SetWindowPos(_hwnd, IntPtr.Zero, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE);
    }

    // ==================== 弹出动画（原型 pop-in 0.28s / pop-out 0.16s） ====================

    private void BuildAnimations()
    {
        var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
        var inDur = new Duration(TimeSpan.FromMilliseconds(280));
        var outDur = new Duration(TimeSpan.FromMilliseconds(160));

        _animIn = BuildAnim(0, 1, 0.96, 1, 6, 0, inDur, ease);
        _animOut = BuildAnim(1, 0, 1, 0.97, 0, 4, outDur, ease);
        _animOut.Completed += (_, _) =>
        {
            _hideAnimating = false;
            // 动画完成回调可能已排队：期间若窗口被重新显示/取消隐藏（代数已变），跳过隐藏
            if (_animHideGen == _hideGen)
                HideNow();
        };

        // 模式切换（主界面 ↔ 设置页）：时长/位移/缩放对齐原型 mode-leave / mode-enter
        var modeOutDur = new Duration(TimeSpan.FromMilliseconds(220));
        var modeInDur = new Duration(TimeSpan.FromMilliseconds(320));
        _modeOutMain = ModeAnim(MainPanel, MainShift, MainScale, 1, 0, 0, -18, 1, 0.98, modeOutDur, ease);
        _modeOutMain.Completed += (_, _) =>
        {
            // 打开设置流程：主页淡出完成 → 隐藏主页，视觉属性复位（此时设置页在上层淡入中）
            MainPanel.Visibility = Visibility.Collapsed;
            ResetModeTransform(MainPanel, MainShift, MainScale);
            _modeAnimating = false;
            // 模式切换完成，拖拽 caption 区域随之切到设置页布局
            UpdateDragRegions();
        };
        _modeInSet = ModeAnim(SettingsPanel, SetShift, SetScale, 0, 1, 22, 0, 0.98, 1, modeInDur, ease);
        _modeOutSet = ModeAnim(SettingsPanel, SetShift, SetScale, 1, 0, 0, 18, 1, 0.98, modeOutDur, ease);
        _modeOutSet.Completed += (_, _) =>
        {
            // 关闭设置流程：设置页淡出完成 → 隐藏设置页，复位（主页在下方淡入中）
            SettingsPanel.Visibility = Visibility.Collapsed;
            ResetModeTransform(SettingsPanel, SetShift, SetScale);
            _modeAnimating = false;
            UpdateDragRegions();
        };
        _modeInMain = ModeAnim(MainPanel, MainShift, MainScale, 0, 1, -22, 0, 0.98, 1, modeInDur, ease);
    }

    /// <summary>构建单个模式切换动画：面板透明度 + 位移 + 缩放（blur 省略：WinUI 无 UIElement 模糊滤镜）。</summary>
    private Storyboard ModeAnim(UIElement target, TranslateTransform shift, ScaleTransform scale,
        double op0, double op1, double x0, double x1, double sc0, double sc1,
        Duration d, EasingFunctionBase ease)
    {
        var sb = new Storyboard();
        void Add(DependencyObject t, string prop, double from, double to)
        {
            var a = new DoubleAnimation { From = from, To = to, Duration = d, EasingFunction = ease };
            Storyboard.SetTarget(a, t);
            Storyboard.SetTargetProperty(a, prop);
            sb.Children.Add(a);
        }
        Add(target, "Opacity", op0, op1);
        Add(shift, "X", x0, x1);
        Add(scale, "ScaleX", sc0, sc1);
        Add(scale, "ScaleY", sc0, sc1);
        return sb;
    }

    /// <summary>复位单个面板的切换视觉状态（隐藏后属性留在动画终值/中间值，恢复显示前必须还原）。</summary>
    private void ResetModeTransform(UIElement panel, TranslateTransform shift, ScaleTransform scale)
    {
        panel.Opacity = 1;
        shift.X = 0;
        scale.ScaleX = scale.ScaleY = 1;
    }

    private Storyboard BuildAnim(double op0, double op1, double sc0, double sc1, double ty0, double ty1,
        Duration d, EasingFunctionBase ease)
    {
        var sb = new Storyboard();
        void Add(DependencyObject target, string prop, double from, double to)
        {
            var a = new DoubleAnimation { From = from, To = to, Duration = d, EasingFunction = ease };
            Storyboard.SetTarget(a, target);
            Storyboard.SetTargetProperty(a, prop);
            sb.Children.Add(a);
        }
        Add(ContentClip, "Opacity", op0, op1);
        Add(_pop, "ScaleX", sc0, sc1);
        Add(_pop, "ScaleY", sc0, sc1);
        Add(_pop, "TranslateY", ty0, ty1);
        return sb;
    }

    private void ResetPop()
    {
        ContentClip.Opacity = 1;
        _pop.ScaleX = _pop.ScaleY = 1;
        _pop.TranslateY = 0;
    }

    // ==================== Host / 托盘 ====================

    private readonly SemaphoreSlim _hostConfigSync = new(1, 1);
    private void QueueHostConfigUpdate(Action<HostConfigUpdate> update)
    {
        _pendingHostConfig ??= new HostConfigUpdate
        {
            HotkeyToggle = LocalState.Ui.Hotkey,
            LaunchOnStartup = LocalState.Ui.LaunchOnStartup,
        };
        update(_pendingHostConfig);
        _ = SyncPendingHostConfigAsync();
    }

    private async Task SyncPendingHostConfigAsync()
    {
        if (!await _hostConfigSync.WaitAsync(0)) return;
        try
        {
            while (true)
            {
                var pending = _pendingHostConfig?.Clone();
                if (pending is null || !await _host.SetConfigAsync(pending)) return;
                await RunOnUiAsync(() =>
                {
                    if (_pendingHostConfig is not null && _pendingHostConfig.Equals(pending))
                        _pendingHostConfig = null;
                });
                if (_pendingHostConfig is null) return;
            }
        }
        finally { _hostConfigSync.Release(); }
    }

    private async Task SyncFromHostAsync()
    {
        var config = await _host.GetConfigAsync();
        if (config is null) return;
        await RunOnUiAsync(() =>
        {
            _syncing = true;
            try
            {
                // 以 LocalState 为权威源：如果 UI 端已持久化的值与 host 不一致，
                // 说明 host 之前没收到推送（连不上/进程重启回滚到旧 config）——
                // 把 LocalState 的值推回 host，而不是用 host 旧值覆盖 LocalState。
                var pushHost = new HostConfigUpdate();
                var changed = false;
                if (LocalState.Ui.Hotkey != config.HotkeyToggle)
                {
                    pushHost.HotkeyToggle = LocalState.Ui.Hotkey;
                    changed = true;
                }
                else
                {
                    LocalState.Ui.Hotkey = config.HotkeyToggle;
                }
                if (LocalState.Ui.LaunchOnStartup != config.LaunchOnStartup)
                {
                    pushHost.LaunchOnStartup = LocalState.Ui.LaunchOnStartup;
                    changed = true;
                }
                else
                {
                    LocalState.Ui.LaunchOnStartup = config.LaunchOnStartup;
                }
                if (LocalState.Ui.LaunchOnStartup != GetStartupEntry())
                    SetStartupEntry(LocalState.Ui.LaunchOnStartup);
                SyncSettingsUi();
                LocalState.SaveUi();
                // LocalState 与 host 有分歧 → 把 LocalState 推回 host（覆盖 host 旧 config）
                if (changed)
                {
                    QueueHostConfigUpdate(x =>
                    {
                        if (pushHost.HotkeyToggle is not null) x.HotkeyToggle = pushHost.HotkeyToggle;
                        if (pushHost.LaunchOnStartup is not null) x.LaunchOnStartup = pushHost.LaunchOnStartup;
                    });
                }
            }
            finally { _syncing = false; }
        });
    }

    private Task RunOnUiAsync(Action action)
    {
        var tcs = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!DispatcherQueue.TryEnqueue(() =>
        {
            try { action(); tcs.SetResult(); }
            catch (Exception ex) { tcs.SetException(ex); }
        }))
            tcs.SetException(new InvalidOperationException("UI dispatcher unavailable"));
        return tcs.Task;
    }

    private async Task MaintainHostConnectionAsync()
    {
        var wasConnected = _host.IsConnected;
        while (true)
        {
            try
            {
                if (!_host.IsConnected)
                {
                    await _host.EnsureConnectedAsync();
                    // 从"未连接"变为"已连接"：首启的 DemoData 兜底已过期，
                    // 自动补一次刷新拿到完整列表（否则要等用户按热键唤醒才看得到）
                    if (_host.IsConnected && !wasConnected)
                    {
                        await SyncPendingHostConfigAsync();
                        if (_pendingHostConfig is null)
                            try { await SyncFromHostAsync(); } catch (Exception ex) { App.Log("HostConfig", ex); }
                        DispatcherQueue.TryEnqueue(() => ScheduleRefresh(QueryBox.Text ?? ""));
                    }
                }
            }
            catch { /* ignore */ }
            wasConnected = _host.IsConnected;
            await Task.Delay(2000);
        }
    }

    /// <summary>host 连接建立（含 UI 冷拉 host 成功）即补查当前输入：连接期间用户敲的词
    /// 已被 DemoData 兜底吞掉显示，不补查的话要等 2s 轮询守护循环才自愈。
    /// 事件可能在任意线程触发，统一 marshal 回 UI 线程。</summary>
    private void OnHostConnected()
    {
        App.Log("Ipc", "host connected; replaying current query");
        DispatcherQueue.TryEnqueue(() => ScheduleRefresh(QueryBox?.Text ?? ""));
    }

    private void OnHostNotification(string method)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            switch (method)
            {                case "ui.show":
                    ShowLauncher();
                    break;
                case "ui.hide":
                    // 保护期内不关
                    if (Environment.TickCount64 < _ignoreDeactivateUntilTicks) return;
                    HideLauncher();
                    break;
                // 注意：没有 "ui.toggle" 分支。toggle 只认 SetEvent（ToggleWatcher）：
                // pipe 广播与事件是两条独立通道，若 pipe 延迟 >300ms 到达会触发第二次
                // HandleToggle，把刚显示的窗口又关掉（"第一次热键没反应、第二次才唤醒"）。
                // Host 端也不再广播 ui.toggle。
            }
        });
    }

    /// <summary>Host 托盘"退出"/host.exit → 整个应用退出（ExitWatcher 事件触发）。</summary>
    private void OnHostExit()
    {
        App.Log("Exit", "exit signal received; exiting");
        // 插件窗口是独立顶层窗口，不随主窗关闭；不先收掉会残留在任务栏。
        PluginWindowHost.CloseAll();
        Application.Current.Exit();
    }

    /// <summary>event + pipe 可能各推一次，300ms 内只处理一次。</summary>
    private void HandleToggle()
    {
        App.Log("Toggle", $"enter: visible={_visible} hideAnim={_hideAnimating}");
        var now = Environment.TickCount64;
        if (now - _lastToggleTicks < 300)
            return;
        _lastToggleTicks = now;

        // 兜底：_visible 与窗口真实可见性可能错位（失焦隐藏路径异常/动画中断/外部隐藏）。
        // 以真实状态为准，避免"窗口已隐藏但 _visible=true"导致快捷键第一次走关闭分支。
        // 判断"视觉不可见"：pop-out 动画播完但 HideNow 未执行时窗口停在淡出态
        // （Opacity≈0、未 SW_HIDE），此时 IsWindowVisible 仍为 true，需一并复位。
        var visuallyHidden = _hwnd != IntPtr.Zero
            && (!IsWindowVisible(_hwnd) || ContentClip.Opacity < 0.1);
        if (visuallyHidden && _visible)
        {
            _visible = false;
            _hideAnimating = false;
            _animOut.Stop();
            ResetPop();
        }

        // 隐藏 pop-out 播放中（失焦关闭中）按快捷键 = 取消关闭：窗口留下并抢回焦点。
        // 否则会走"关闭"分支（_visible 尚未复位），导致"按一次像关闭，要按两次才唤起"。
        if (_hideAnimating)
        {
            _hideGen++;  // 使已排队的 _animOut.Completed→HideNow 失配
            _hideAnimating = false;
            _animOut.Stop();
            ResetPop();
            _visible = true;
            ForceForeground();
            Activate();
            QueryBox.Focus(FocusState.Keyboard);
            ResetIme();
            // ForceForeground 不再做同步重试（见其注释）：这里补异步重试兜底，
            // 确保真拿到前台（否则点击外面关不掉）。
            _ = RetryFocusAsync();
            // 失焦到重新抢回前台的链路同样挂不上 TSF（程序化聚焦无效），
            // 补一次真实输入激活（组词进行中会自动跳过，见 KickImeAsync）。
            _ = KickImeAsync();
            return;
        }

        if (_visible)
        {
            // 跨屏跟随（uTools 式）：窗口可见时按热键，鼠标不在窗口所在屏
            // → 移到鼠标屏（不关闭）；想关闭时鼠标应在窗口所在屏再按热键。
            if (!CursorOnWindowMonitor())
            {
                MoveToCursorMonitor();
                return;
            }
            HideLauncher(byToggle: true);
        }
        else
            ShowLauncher();
    }

    /// <summary>鼠标是否在窗口所在显示器上（跨屏跟随判断用）。
    /// 用 MonitorFromPoint 比较鼠标点与窗口中心的显示器归属。</summary>
    private bool CursorOnWindowMonitor()
    {
        if (_hwnd == IntPtr.Zero) return true;
        GetCursorPos(out var pt);
        GetWindowRect(_hwnd, out var wr);
        var winPt = new POINT { X = (wr.Left + wr.Right) / 2, Y = (wr.Top + wr.Bottom) / 2 };
        return MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)
            == MonitorFromPoint(winPt, MONITOR_DEFAULTTONEAREST);
    }

    /// <summary>强制 backdrop 重连：让 DWM 重新模糊当前窗口位置后的屏幕内容。
    /// 唤起/跨屏后 Acrylic 模糊可能停留在旧位置（玻璃呈"黑色"），重连即刷新。</summary>
    private void ReconnectBackdrop()
    {
        try
        {
            SystemBackdrop = null;
            SystemBackdrop = _acrylicBackdrop;
            bool dark = LocalState.Ui.Theme switch
            {
                "light" => false,
                "dark" => true,
                _ => SystemUsesDark(),
            };
            _acrylicBackdrop.ApplyTheme(dark);
            App.Log("Backdrop", "reconnected");
        }
        catch (Exception ex) { App.Log("Backdrop", ex); }
    }

    /// <summary>窗口可见时跨屏跟随：隐藏→重新显示在鼠标所在屏。
    /// WinUI 3 中"已存在的 TextBox"跨屏 Move 后，TSF 文档管理器无法通过程序化
    /// 聚焦重新挂接（只有真实输入事件能激活——用户实测点击才恢复）；而"隐藏→
    /// 显示"会走完整显示周期（重建输入上下文 + 焦点往返），输入法必然恢复。
    /// 注意：AppWindow.Hide/Show 是异步状态机，Hide+Show 同帧连续调用会合并，
    /// Show 的激活/输入上下文部分被跳过（窗口显示但 TSF 不挂）——必须延迟到
    /// 状态机处理完隐藏后再 Show。视觉上窗口从旧屏消失、在鼠标屏带 pop-in
    /// 重新弹出；保留已输入文本。</summary>
    private void MoveToCursorMonitor()
    {
        App.Log("CrossScreen", "move via hide+show (delayed)");
        var text = QueryBox.Text ?? "";
        _animOut.Stop();
        HideNow();
        _ = Task.Delay(100).ContinueWith(_ => DispatcherQueue.TryEnqueue(() =>
        {
            if (_visible) return;  // 期间用户已重新显示，跳过重复 Show
            ShowLauncher();
            // ShowLauncher 按"唤起"语义清空输入；跨屏跟随应保留已输入内容继续输入。
            // 注意：给 Text 赋值后插入光标默认回到开头，需显式移到末尾（继续输入=追加）
            if (!string.IsNullOrEmpty(text))
            {
                QueryBox.Text = text;
                _composing = false;  // 程序化替换文本已销毁组词；防 Ended 不触发留下 stale 标记
                QueryBox.SelectionStart = text.Length;
                QueryBox.SelectionLength = 0;
                _ = RefreshResultsAsync(text);
            }
            // 跨屏后 TSF 文档管理器只有真实输入事件能重新激活（WinUI 3 框架 bug，
            // 程序化聚焦/往返全部无效）——ShowLauncher 内置的 KickImeAsync 已在显示
            // 后 150ms 注入一次真实点击，这里无需重复（300ms 去重兜底连点）。
        }));
    }

    /// <summary>显示/抢回焦点稳定后注入一次真实指针输入恢复输入法（TSF 文档管理器
    /// 激活）。抑制条件只有"组词进行中"（_composing）：组词活着 = TSF 已挂接，
    /// 无需救活且点击会打断组词；而"文本非空但无组词"恰恰是 TSF 挂接失败的表现
    /// （坏状态下按键直接落明文），必须注入。300ms 内不重复注入（连点会被识别为
    /// 双击选词）。落点校验/换算见 InjectImeKickClick。</summary>
    private async Task KickImeAsync()
    {
        try
        {
            await Task.Delay(150);  // 等窗口显示稳定（布局就绪）
            if (!_visible || _hwnd == IntPtr.Zero)
                return;
            if (_composing)
                return;
            if (Environment.TickCount64 - _lastImeKickTicks < 300)
                return;
            if (InjectImeKickClick())
                _lastImeKickTicks = Environment.TickCount64;
        }
        catch (Exception ex) { App.Log("ImeKick", ex); }
    }

    /// <summary>注入一次真实指针输入激活 TSF：优先触摸注入（InputInjector 走
    /// WM_POINTER 指针管道，WinUI 3 XAML 原生识别，且不移动光标）；
    /// mouse_event 经典鼠标消息不被 XAML 识别（实测 Pointer 事件不产生），
    /// 仅作回退。落点由 QueryBox 实时布局换算（DPI 缩放/留白调整免疫，不再用
    /// "窗口顶+40px"硬编码——高 DPI 下会脱靶）。注入前校验落点确实命中本窗口：
    /// 系统级注入点到别的应用等于替用户点击别人，窗口被隐藏/被遮挡时必须放弃。
    /// 返回是否实际注入。</summary>
    private bool InjectImeKickClick()
    {
        try
        {
            if (_hwnd == IntPtr.Zero) return false;
            if (!GetWindowRect(_hwnd, out var wr)) return false;

            var cx = (wr.Left + wr.Right) / 2;
            var cy = wr.Top + 40;  // 布局换算失败的回退落点（输入行近似位置）
            try
            {
                // QueryBox 中心（Root 坐标 × DPI 缩放 + 窗口原点）= 物理屏幕坐标
                var pt = QueryBox.TransformToVisual(Root)
                    .TransformPoint(new Point(QueryBox.ActualWidth / 2, QueryBox.ActualHeight / 2));
                var scale = Root.XamlRoot?.RasterizationScale ?? 1.0;
                cx = wr.Left + (int)(pt.X * scale);
                cy = wr.Top + (int)(pt.Y * scale);
            }
            catch (Exception ex)
            {
                App.Log("Ime", $"querybox point fallback: {ex.Message}");
            }

            // 落点必须命中本窗口（XAML 桥接子窗口的根也是 _hwnd）。
            // _visible 字段可能与真实状态错位（Show 失败/外部隐藏），
            // 隐藏窗口的 GetWindowRect 仍返回旧矩形——不校验就会注入到别人窗口上。
            var hit = WindowFromPoint(new POINT { X = cx, Y = cy });
            if (hit == IntPtr.Zero || GetAncestor(hit, GA_ROOT) != _hwnd)
            {
                App.Log("Ime", $"kick skipped: hit 0x{hit.ToInt64():X} at ({cx},{cy}) is not self (visible={IsWindowVisible(_hwnd)})");
                return false;
            }

            if (TryInjectTouchClick(cx, cy))
            {
                App.Log("Ime", $"touch injected at ({cx},{cy}) wr=({wr.Left},{wr.Top},{wr.Right},{wr.Bottom})");
                return true;
            }
            // 回退：SetCursorPos + mouse_event（经典消息，XAML 可能不识别）
            GetCursorPos(out var cur);
            SetCursorPos(cx, cy);
            mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, IntPtr.Zero);
            mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, IntPtr.Zero);
            SetCursorPos(cur.X, cur.Y);
            App.Log("Ime", $"mouse fallback at ({cx},{cy})");
            return true;
        }
        catch (Exception ex)
        {
            App.Log("ImeKick", ex);
            return false;
        }
    }

    /// <summary>InputInjector 触摸注入（WM_POINTER 管道，XAML 原生输入路径）。
    /// 成功返回 true；不可用返回 false 走 mouse 回退。</summary>
    private bool TryInjectTouchClick(int x, int y)
    {
        try
        {
            var injector = InputInjector.TryCreate();
            if (injector is null)
            {
                App.Log("Ime", "InputInjector.TryCreate -> null");
                return false;
            }
            // 触摸注入坐标是 DIP：物理像素除以窗口所在屏的缩放
            var scale = Root.XamlRoot?.RasterizationScale ?? 1.0;
            var dx = (int)(x / scale);
            var dy = (int)(y / scale);
            var pos = new InjectedInputPoint { PositionX = dx, PositionY = dy };
            var down = new InjectedInputTouchInfo
            {
                PointerInfo = new InjectedInputPointerInfo
                {
                    PointerId = 1,
                    PixelLocation = pos,
                    PointerOptions = InjectedInputPointerOptions.New
                        | InjectedInputPointerOptions.PointerDown
                        | InjectedInputPointerOptions.InContact,
                    PerformanceCount = 0,
                },
                Contact = new InjectedInputRectangle { Left = dx - 2, Top = dy - 2, Right = dx + 2, Bottom = dy + 2 },
            };
            injector.InjectTouchInput(new[] { down });
            var up = new InjectedInputTouchInfo
            {
                PointerInfo = new InjectedInputPointerInfo
                {
                    PointerId = 1,
                    PixelLocation = pos,
                    PointerOptions = InjectedInputPointerOptions.PointerUp,
                    PerformanceCount = 0,
                },
            };
            injector.InjectTouchInput(new[] { up });
            return true;
        }
        catch (Exception ex)
        {
            App.Log("Ime", $"touch inject failed: {ex.Message}");
            return false;
        }
    }

    private static string? FindIconPath()
    {
        var names = new[] { "Assets\\spark.ico", "spark.ico" };
        var bases = new[]
        {
            AppContext.BaseDirectory,
            Directory.GetCurrentDirectory(),
            Path.GetDirectoryName(Environment.ProcessPath) ?? ""
        };
        foreach (var b in bases)
        foreach (var n in names)
        {
            var p = Path.Combine(b, n);
            if (File.Exists(p)) return p;
        }
        return null;
    }

    private void SetupChrome()
    {
        _hwnd = WindowNative.GetWindowHandle(this);
        var id = Win32Interop.GetWindowIdFromWindow(_hwnd);
        _appWindow = AppWindow.GetFromWindowId(id);
        _appWindow.Title = "Spark";
        var icon = FindIconPath();
        if (icon is not null)
        {
            try { _appWindow.SetIcon(icon); } catch (Exception ex) { App.Log("SetIcon", ex); }
        }

        if (_appWindow.Presenter is OverlappedPresenter p)
        {
            p.IsResizable = false;
            p.IsMaximizable = false;
            p.IsMinimizable = false;
            p.SetBorderAndTitleBar(false, false);
            p.IsAlwaysOnTop = true;
        }
        try { _appWindow.IsShownInSwitchers = false; } catch { /* older SDK */ }

        try
        {
            const int GWL_EXSTYLE = -20;
            const int WS_EX_TOOLWINDOW = 0x00000080;
            const int WS_EX_APPWINDOW = 0x00040000;
            var ex = GetWindowLong(_hwnd, GWL_EXSTYLE);
            ex = (ex | WS_EX_TOOLWINDOW) & ~WS_EX_APPWINDOW;
            SetWindowLong(_hwnd, GWL_EXSTYLE, ex);
        }
        catch (Exception ex) { App.Log("ToolWindow", ex); }

        try
        {
            // DWM 圆角：窗口形状由 DWM 按系统设置裁圆角（默认 ~8px，实测本机约 5px，不可按窗口调大），
            // 圆角外的楔形区属于窗口外、显示桌面背景。壁纸/玻璃/内容层全部收在
            // ContentClip(CornerRadius=WinCornerRadius=5) 内与 DWM 弧线重合——XAML 圆角大于 DWM 圆角时
            // 两弧之间的楔形区会露出 Root 的壁纸基底（近黑 #0D1220），就是四角的黑贴片。
            const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
            const int DWMWCP_ROUND = 2;
            var pref = DWMWCP_ROUND;
            DwmSetWindowAttribute(_hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref pref, sizeof(int));

            // 客户区填满整个窗口，避免 DWM 在四周留出系统边距
            const int DWMWA_ALLOW_CLIENT_AREA_TO_FILL_ENTIRE_WINDOW = 14;
            var allow = 1;
            DwmSetWindowAttribute(_hwnd, DWMWA_ALLOW_CLIENT_AREA_TO_FILL_ENTIRE_WINDOW, ref allow, sizeof(int));
        }
        catch { /* ignore */ }

        MakeFrameless();
        // 双击 caption 拖拽区会最大化（全屏）：挂子类吞掉该消息（见 NoMaximizeWndProc）
        try { SetWindowSubclass(_hwnd, _noMaximizeProc, new UIntPtr(CaptionSubclassId), IntPtr.Zero); }
        catch (Exception ex) { App.Log("WindowSubclass", ex); }
        PlaceWindow(LauncherWidth, 590);

        // 前台变化钩子：点击外面/Alt+Tab 的本质是前台从本窗口切走。
        // 不依赖 WinUI 的 Activated 事件（该事件在部分环境下不可靠/不触发），
        // 用 Win32 EVENT_SYSTEM_FOREGROUND 精确感知"失焦"。
        try
        {
            _fgHook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND,
                IntPtr.Zero, _fgHookProc, 0, 0, WINEVENT_OUTOFCONTEXT);
        }
        catch (Exception ex) { App.Log("WinEventHook", ex); }
    }

    /// <summary>前台窗口变化（全局钩子回调，钩子线程）。</summary>
    private void OnForegroundChanged(IntPtr hook, uint evt, IntPtr hwnd, int idObject, int idChild,
        uint dwEventThread, uint dwmsEventTime)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            if (hwnd == _hwnd) return;  // 前台变成自己（打开/抢焦点）
            if (!_visible) return;
            if (Environment.TickCount64 < _ignoreDeactivateUntilTicks) return;
            if (SettingsPanel.Visibility == Visibility.Visible) return;
            HideLauncher();
            App.Log("Focus", $"fg hook -> hide (fg=0x{hwnd.ToInt64():X})");
        });
    }

    private void SetupTray()
    {
        // Host 已接管托盘时可不建；未连 Host 时保留 UI 托盘便于调试
        if (_host.IsConnected) return;
        if (_hwnd == IntPtr.Zero)
            _hwnd = WindowNative.GetWindowHandle(this);
        var icon = FindIconPath() ?? "";
        _tray?.Dispose();
        _tray = new TrayService(
            _hwnd,
            icon,
            onShow: () => DispatcherQueue.TryEnqueue(ShowLauncher),
            onExit: () => DispatcherQueue.TryEnqueue(() =>
            {
                _tray?.Dispose();
                _tray = null;
                Close();
                Environment.Exit(0);
            }));
    }

    /// <summary>把窗口放到鼠标所在显示器（uTools 式唤起：鼠标在哪块屏，窗口就弹在哪块屏）。</summary>
    private void PlaceWindow(int w, int h)
    {
        if (_appWindow is null) return;
        _appWindow.Resize(new SizeInt32(w, h));
        _appWindow.Title = "Spark";
        _appWindow.Move(CursorPlacement(w, h));
    }

    /// <summary>
    /// 计算唤起位置：鼠标所在显示器工作区水平居中、垂直 1/6（对齐原型顶部留白比例）。
    /// GetCursorPos 在 PerMonitorV2 下返回物理像素，与 GetMonitorInfo/AppWindow.Move 同一坐标系，
    /// 跨屏 DPI 无换算问题。Win32 取屏失败时回退主屏居中（旧行为）。
    /// </summary>
    private PointInt32 CursorPlacement(int w, int h)
    {
        if (GetCursorPos(out var pt))
        {
            var hMon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            var mi = new MONITORINFO { cbSize = Marshal.SizeOf<MONITORINFO>() };
            if (hMon != IntPtr.Zero && GetMonitorInfo(hMon, ref mi))
            {
                var work = mi.rcWork;
                var workW = work.Right - work.Left;
                var workH = work.Bottom - work.Top;
                var pos = new PointInt32(
                    work.Left + (workW - w) / 2,
                    work.Top + Math.Max(80, workH / 6));
                App.Log("Cursor", $"mouse=({pt.X},{pt.Y}) mon work=({work.Left},{work.Top},{work.Right},{work.Bottom}) -> ({pos.X},{pos.Y})");
                return pos;
            }
            App.Log("Cursor", $"GetMonitorInfo failed hMon={hMon.ToInt64():X}");
        }
        else
        {
            App.Log("Cursor", "GetCursorPos failed");
        }

        // 兜底：主屏工作区居中（旧行为）
        var appWindow = _appWindow;
        if (appWindow is null) return default;
        var area = DisplayArea.GetFromWindowId(appWindow.Id, DisplayAreaFallback.Primary);
        if (area is null) return appWindow.Position;
        return new PointInt32(
            area.WorkArea.X + (area.WorkArea.Width - w) / 2,
            area.WorkArea.Y + Math.Max(80, area.WorkArea.Height / 6));
    }

    // ==================== 拖拽 ====================

    /// <summary>
    /// 用 InputNonClientPointerSource 把"空白处"声明为标题栏区域（caption），系统原生拖动。
    /// 只覆盖无交互控件的地方：主界面顶部留白条 + 底部整条底栏；设置页顶部留白条。
    /// 用 WinUI 原生机制（1.6 起 XAML 内容在 DesktopChildSiteBridge 子窗口里，
    /// WM_NCLBUTTONDOWN 伪装标题栏点击会失灵，所以不走 SendMessage 方案）。
    /// </summary>
    private void UpdateDragRegions()
    {
        if (_appWindow is null || Root.XamlRoot is null) return;
        var scale = Root.XamlRoot.RasterizationScale;
        var w = (int)Math.Round(Root.ActualWidth * scale);
        var h = (int)Math.Round(Root.ActualHeight * scale);
        var rects = new List<RectInt32>();

        if (MainPanel.Visibility == Visibility.Visible)
        {
            // 主界面：搜索行顶部 16px 留白全宽 + 底栏整条（chips/文字均不可交互）
            rects.Add(new RectInt32(0, 0, w, (int)Math.Round(16 * scale)));
            if (FooterBar.ActualHeight > 0
                && FooterBar.TransformToVisual(Root).TransformPoint(new Point(0, 0)) is Point fp)
            {
                rects.Add(new RectInt32(0, (int)Math.Round(fp.Y * scale), w,
                    (int)Math.Round(FooterBar.ActualHeight * scale)));
            }
        }
        else
        {
            // 设置页：顶栏 14px 留白全宽
            rects.Add(new RectInt32(0, 0, w, (int)Math.Round(14 * scale)));
        }
        Microsoft.UI.Input.InputNonClientPointerSource.GetForWindowId(_appWindow.Id)
            .SetRegionRects(Microsoft.UI.Input.NonClientRegionKind.Caption, rects.ToArray());
    }

    // ==================== 标题栏双击防最大化 ====================

    /// <summary>窗口子类钩子：双击伪标题栏（caption 区）会被系统当作"最大化"（全屏）。
    /// Presenter.IsMaximizable=false 挡不住全部路径（WinUI 显示时会重写窗口样式，最大化样式位可能被加回来），
    /// 直接在子类里吞掉 WM_NCLBUTTONDBLCLK(HTCAPTION)：消息不进 DefWindowProc 就不会发 SC_MAXIMIZE。
    /// 子类用 SetWindowSubclass 链式挂接，不覆盖 WinUI 自己的窗口过程。</summary>
    private readonly SUBCLASSPROC _noMaximizeProc = NoMaximizeWndProc;

    private static IntPtr NoMaximizeWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam,
        IntPtr uIdSubclass, IntPtr dwRefData)
    {
        // wParam = 命中测试码；HTCAPTION=2（caption 拖拽区双击）
        if (msg == WM_NCLBUTTONDBLCLK && wParam.ToInt32() == HTCAPTION)
            return IntPtr.Zero;  // 吞掉：不转默认处理
        return DefSubclassProc(hWnd, msg, wParam, lParam, uIdSubclass, dwRefData);
    }

    // ==================== 显示 / 隐藏 ====================

    public void ShowLauncher()
    {
        var _showSw = System.Diagnostics.Stopwatch.StartNew();
        try
        {
            // 先标记可见 + 保护期，再 Show，避免中间 Deactivated 立刻 Hide
            _visible = true;
            _ignoreDeactivateUntilTicks = Environment.TickCount64 + 500;

            if (_hwnd == IntPtr.Zero)
                _hwnd = WindowNative.GetWindowHandle(this);

            // 窗口仍隐藏：先移到目标屏（鼠标所在屏），再采样背景色。
            // SyncDwmBorderColor 按窗口当前矩形采样屏幕均值算 1px 边框感知色，
            // 跨屏唤起时若在旧位置采样，边框色会按上一块屏算（白屏↔黑屏切换时颜色反了）。
            // SetWindowPos 对隐藏窗口立即生效（AppWindow.Move 在未显示时行为不定）。
            if (_appWindow is not null)
            {
                var p = CursorPlacement(LauncherWidth, 590);
                SetWindowPos(_hwnd, IntPtr.Zero, p.X, p.Y, 0, 0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            // 采样背景把 DWM 圆角外框设成当前玻璃感知色（详见 SyncDwmBorderColor）。
            // 必须同步在 Show 之前、隐藏态执行：采样内容 = "窗口矩形处的桌面背景"，
            // 窗口可见后矩形里就是窗口自身（玻璃+内容），把采样挪后台会与下面的 Show
            // 形成竞态、随时序采到启动器自己（审计 NEEDS_FIX #1 修复：恢复同步 +
            // 打点验证开销）。SetWindowPos 已在上面先行生效，跨屏采样顺序保持不变。
            var _borderSw = System.Diagnostics.Stopwatch.StartNew();
            try { SyncDwmBorderColor(); } catch { /* ignore */ }
            App.Log("Startup", $"SyncDwmBorderColor {_borderSw.ElapsedMilliseconds}ms");

            try { _appWindow?.Show(true); } catch { /* ignore */ }
            ShowWindow(_hwnd, 9);  // SW_RESTORE
            ShowWindow(_hwnd, 5);  // SW_SHOW

            // Show 之后 Resize 才生效（窗口未显示时 Resize 会被忽略）
            PlaceWindow(LauncherWidth, 590);
            // 防止 WinUI 在显示时把 DLGFRAME 样式加回来（白圈来源）
            try { MakeFrameless(); } catch { /* ignore */ }

            ForceForeground();
            Activate();
            // 唤起总是回到主页：设置页可能在上次关闭前处于打开状态；
            // 瞬时切换同时复位模式动画可能停在中间值的透明度/位移
            ApplyModeSwitch(open: false, animate: false);
            QueryBox.Text = "";
            _composing = false;  // 清空文本已销毁组词会话；Ended 事件未必触发，防 stale 挡住 kick
            // 组词中 ScheduleRefresh 早退不更新该字段，此处程序化清空要显式复位，
            // 否则唤起后第一个字符误走 80ms 防抖而非首字符立即通道
            _lastScheduledQuery = "";
            _ = RefreshResultsAsync("");
            // Text="" 已触发 OnQueryChanged → ScheduleRefresh（80ms 防抖）；唤起语义
            // 固定查空串，上面显式这发已覆盖，取消防抖那次避免每次唤起双重 IPC 查询。
            // （组词中赋值时 ScheduleRefresh 早退、无新 CTS，取消的是旧句柄，无害。）
            _debounceCts?.Cancel();
            // 用 Keyboard 状态聚焦 + IME 重建：缓解 Show/Hide 循环后中文输入法
            // 候选窗不弹（详见 ResetIme）。注意：仅靠程序化聚焦+重建不足以挂上
            // TSF 文档管理器，真正的修复在下面的 KickImeAsync（注入真实输入）。
            QueryBox.Focus(FocusState.Keyboard);
            ResetIme();
            LogFocusState("show");
            // 唤起时收藏选中态复位（上一轮导航状态不残留）
            _favActive = -1;
            UpdateFavCardStates();
            // 前台锁偶发拦截 SetForegroundWindow（窗口显示但未激活 → 点击外面不会触发失焦隐藏）。
            // 延迟重试几次，确保窗口真正拿到前台。
            _ = RetryFocusAsync();
            // 热键/托盘唤起路径与跨屏同患：TSF 文档管理器挂不上（WinUI 3 框架 bug，
            // 程序化聚焦/焦点往返/IMM 重建全部无效，只有真实指针输入能激活，详见
            // MoveToCursorMonitor 注释），表现为中文输入法候选框不弹、鼠标点一下
            // 搜索框才恢复。注入一次真实点击救活，用户无需手动点。
            _ = KickImeAsync();

            // pop-in（对齐原型 .launcher 入场）
            _hideGen++;  // 使已排队的隐藏动画 Completed→HideNow 失配，防止刚显示的窗口被延迟关掉
            _hideAnimating = false;
            _animOut.Stop();
            ContentClip.Opacity = 0;
            _pop.ScaleX = _pop.ScaleY = 0.96;
            _pop.TranslateY = 6;
            _animIn.Begin();

            // 焦点落稳后再延长一点保护。注意不能太长：用户可能马上点击外面
            // 触发失焦隐藏，保护期过长会把这真实的点击也拦掉（表现为点击外面
            // 没隐藏、第一次热键变"关闭"）。400ms 足够覆盖抢前台过程的瞬态。
            _ignoreDeactivateUntilTicks = Environment.TickCount64 + 400;
            // 首帧前的同步块总耗时（诊断唤起延迟：动画之前这段越短首帧越快）
            App.Log("Startup", $"ShowLauncher body {_showSw.ElapsedMilliseconds}ms");
        }
        catch (Exception ex)
        {
            App.Log("ShowLauncher", ex);
        }
    }

    /// <summary>强制重建输入法上下文：窗口 Show/Hide 循环后，XAML 的输入法（TSF）
    /// 上下文可能没重新挂到搜索框上——中文输入法候选窗不弹、只能打英文。
    /// 真实鼠标点击能恢复（pointer 交互才会激活 TSF 文档管理器），程序化聚焦不会，
    /// 所以只能靠重建输入上下文触发输入服务重新桥接。
    /// 注意：WinUI 3 的 XAML 内容在桥接子窗口里，键盘焦点窗口（GetFocus）才是输入
    /// 上下文所在窗口，对顶层 _hwnd 操作无效。必须在窗口显示且搜索框聚焦后调用（UI 线程）。</summary>
    private void ResetIme()
    {
        try
        {
            // XAML 桥接子窗口持键盘焦点；GetFocus 失败时回退顶层窗口
            var focusHwnd = GetFocus();
            if (focusHwnd == IntPtr.Zero) focusHwnd = _hwnd;
            if (focusHwnd == IntPtr.Zero) return;

            // 摘除再挂回：强制 IMM32 上下文重建（对 TSF 输入法走兼容层同样生效）
            var prev = ImmAssociateContext(focusHwnd, IntPtr.Zero);
            if (prev != IntPtr.Zero)
                ImmAssociateContext(focusHwnd, prev);

            var himc = ImmGetContext(focusHwnd);
            if (himc != IntPtr.Zero)
            {
                try
                {
                    ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                    ImmSetOpenStatus(himc, true);
                }
                finally
                {
                    ImmReleaseContext(focusHwnd, himc);
                }
            }
            // CPS_CANCEL 程序化取消组词时 TextCompositionEnded 未必触发，
            // 显式复位防 stale 的 _composing 把 KickImeAsync 永久挡住
            _composing = false;
        }
        catch { /* ignore */ }
    }

    /// <summary>显示后延迟重试抢前台 + 补聚焦，直到窗口确实处于前台或状态变化。
    /// 必须拿到前台：未激活的窗口点击外面时前台无变化，FgHook/Deactivated 都不触发，
    /// "点击外面隐藏"会失效（用户实测：唤起未抢到前台时点外面关不掉）。
    /// 每次重试都补一次输入框聚焦 + IME 重建：TSF 文档管理器只在窗口激活稳定后才挂上，
    /// 且需要多轮（第一轮焦点可能被动画/抢前台吃掉）。已拿到前台也至少跑两轮再停。</summary>
    private async Task RetryFocusAsync()
    {
        for (var i = 0; i < 5; i++)
        {
            await Task.Delay(120);
            if (!_visible || _hwnd == IntPtr.Zero)
                return;
            // 第一轮（窗口已显示稳定）：强制 backdrop 重连刷新 Acrylic 采样。
            // 唤起/跨屏后 DWM 模糊可能停留在旧位置/旧屏（玻璃呈"黑色"），
            // 点击触发重绘才恢复——这里主动重连，无需用户点击。
            if (i == 0)
                ReconnectBackdrop();
            if (GetForegroundWindow() != _hwnd)
            {
                ForceForeground();
                Activate();
            }
            // 焦点往返：把焦点先移出输入框再移回，强制触发真正的 LostFocus→GotFocus。
            // TSF 文档管理器只在 GotFocus 时尝试挂接：唤起瞬间窗口尚未激活，首次
            // GotFocus 挂接会失败；而焦点已在输入框时重复 Focus() 是 no-op（不触发
            // GotFocus），所以必须移走焦点再移回，让窗口激活稳定后的这次 GotFocus
            // 重新走 TSF 挂接（中文输入法候选窗才能弹出）。
            ResultList.Focus(FocusState.Programmatic);
            QueryBox.Focus(FocusState.Keyboard);
            ResetIme();
            LogFocusState($"retry{i}");
            // 已拿到前台且至少补了四轮（跨屏 DPI 重建可能较久，TSF 挂接需要窗口稳定）
            if (GetForegroundWindow() == _hwnd && i >= 3)
                return;
        }
    }

    /// <summary>记录唤起后焦点/输入法状态（排查"中文输入法不弹候选窗"用）：
    /// 窗口是否前台、Win32 键盘焦点窗口、XAML 焦点元素、IMM 输入法开关状态。</summary>
    private void LogFocusState(string tag)
    {
        try
        {
            var fg = GetForegroundWindow();
            var focus = GetFocus();
            var xamlFocus = FocusManager.GetFocusedElement(Root.XamlRoot);
            var xamlName = (xamlFocus as FrameworkElement)?.Name
                ?? (xamlFocus as TextBox)?.Name ?? xamlFocus?.GetType().Name ?? "null";

            var imeOpen = false;
            var focusHwnd = focus != IntPtr.Zero ? focus : _hwnd;
            if (focusHwnd != IntPtr.Zero)
            {
                var himc = ImmGetContext(focusHwnd);
                if (himc != IntPtr.Zero)
                {
                    imeOpen = ImmGetOpenStatus(himc);
                    ImmReleaseContext(focusHwnd, himc);
                }
            }
            App.Log("Focus", $"{tag}: fg={(fg == _hwnd)} focus=0x{focus.ToInt64():X} xaml={xamlName} imeOpen={imeOpen}");
        }
        catch (Exception ex) { App.Log("FocusState", ex); }
    }

    public void HideLauncher()
    {
        HideLauncher(byToggle: false);
    }

    /// <summary>
    /// byToggle=true：热键主动隐藏，绕过保护期——保护期只防被动隐藏源
    /// （FgHook/Deactivated 在显示后短时间内误杀刚弹出的窗口），不能吞掉
    /// 用户明确按热键的隐藏意图（否则"显示后短时间内按热键没反应"）。
    /// </summary>
    public void HideLauncher(bool byToggle)
    {
        // 保护期内禁止隐藏（防闪关）
        if (!byToggle && Environment.TickCount64 < _ignoreDeactivateUntilTicks && _visible)
        {
            App.Log("Focus", $"hide blocked by guard ({_ignoreDeactivateUntilTicks - Environment.TickCount64}ms left)");
            return;
        }
        if (!_visible)
        {
            App.Log("Focus", "hide skipped: !_visible");
            return;
        }

        // pop-out（对齐原型 .launcher.closing），完成后才真正隐藏
        _hideAnimating = true;
        _animHideGen = ++_hideGen;
        _animIn.Stop();
        _animOut.Begin();
    }
    private void HideNow()
    {
        // 取消未结束的 IME 组词：隐藏时残留的组合状态会让下次显示时输入法失灵
        // （中文模式直接打出英文），同时复位 _composing 避免箭头键逻辑误判。
        try
        {
            if (_hwnd != IntPtr.Zero)
            {
                var himc = ImmGetContext(_hwnd);
                if (himc != IntPtr.Zero)
                {
                    ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                    ImmReleaseContext(_hwnd, himc);
                }
            }
        }
        catch { /* ignore */ }
        _composing = false;

        // 拖拽后的位置在隐藏时落盘（拖动过程不逐帧写盘；仅窗口真正显示过才会走到这里）
        if (_appWindow is not null)
        {
            var pos = _appWindow.Position;
            if (pos.X != LocalState.Ui.WindowX || pos.Y != LocalState.Ui.WindowY)
            {
                LocalState.Ui.WindowX = pos.X;
                LocalState.Ui.WindowY = pos.Y;
                LocalState.SaveUi();
            }
        }
        try { _appWindow?.Hide(); } catch { /* ignore */ }
        try
        {
            if (_hwnd != IntPtr.Zero)
                ShowWindow(_hwnd, 0); // SW_HIDE
        }
        catch { /* ignore */ }
        _visible = false;
        ResetPop();
    }

    private void ForceForeground()
    {
        try
        {
            if (_hwnd == IntPtr.Zero)
                _hwnd = WindowNative.GetWindowHandle(this);

            // 前台锁绕过：模拟一次 Alt 键让本进程获得"最近输入"资格，
            // 否则点击外面后（输入权在别的进程）SetForegroundWindow 会被拒，
            // 窗口显示后保护期一过就被 FgHook 关掉（"第一次热键没反应"）。
            keybd_event(0x12 /*VK_MENU*/, 0, 0x0001 /*KEYEVENTF_EXTENDEDKEY*/, UIntPtr.Zero);
            keybd_event(0x12, 0, 0x0001 | 0x0002 /*KEYEVENTF_KEYUP*/, UIntPtr.Zero);

            // AttachThreadInput 技巧，提高 SetForegroundWindow 成功率
            var fg = GetForegroundWindow();
            var fgTid = GetWindowThreadProcessId(fg, out _);
            var curTid = GetCurrentThreadId();
            if (fgTid != curTid)
                AttachThreadInput(fgTid, curTid, true);

            SetForegroundWindow(_hwnd);
            // 当前线程的窗口可绕过前台锁直接激活（SetForegroundWindow 可能被拒）；
            // 激活状态不完整时 TSF 输入上下文不会挂上（输入法候选窗不弹），
            // 真实点击能恢复也是因为点击会完成激活链路。
            SetActiveWindow(_hwnd);
            BringWindowToTop(_hwnd);
            SetWindowPos(_hwnd, new IntPtr(-1), 0, 0, 0, 0, 0x0001 | 0x0002); // HWND_TOPMOST
            SetWindowPos(_hwnd, new IntPtr(-2), 0, 0, 0, 0, 0x0001 | 0x0002); // HWND_NOTOPMOST
            // 保持 AlwaysOnTop 由 OverlappedPresenter 管；这里只是抢焦点

            // 抢前台的失败重试不在这里同步做（Thread.Sleep 会卡 UI 线程、排在首帧前），
            // 统一交给调用方跟随的 RetryFocusAsync（异步多轮 + 每轮补聚焦/IME 重建）。
            // 拿不到前台 = 窗口未激活：点击外面时前台无变化，FgHook/Deactivated 都不
            // 触发，窗口无法靠"点击外面"隐藏——所以每个 ForceForeground 调用点必须
            // 保证有 RetryFocusAsync 兜底。

            if (fgTid != curTid)
                AttachThreadInput(fgTid, curTid, false);
        }
        catch { /* ignore */ }
    }

    // ==================== 搜索结果 ====================

    private async Task RefreshResultsAsync(string q)
    {
        var gen = Interlocked.Increment(ref _queryGen);
        QueryResultDto result;
        try { result = await _host.QueryAsync(q); }
        catch (Exception ex)
        {
            App.Log("Query", ex);
            result = DemoData.Query(q);
        }
        if (gen != _queryGen) return;

        ApplyResults(result, gen, q);

        // host 报 partial（native 插件结果未就绪/超时）：延迟补查一次。
        // 防风暴：同一 q 只补一次（PartialRequeryQuery 去重）；用户继续输入会推进
        // _queryGen，补查自然作废（gen 守卫丢弃）。空查询时复位去重标记——
        // 覆盖"删光后重打同一个词"：上一轮补查可能已被 gen 失配丢弃，重打应重新可补。
        if (q.Length == 0)
            PartialRequeryQuery = null;
        if (result.Partial && PartialRequeryQuery != q)
        {
            PartialRequeryQuery = q;
            await Task.Delay(PartialRequeryDelayMs);
            if (gen == _queryGen)
                await RefreshResultsAsync(q);
        }
    }

    /// <summary>partial 补查去重：最近一次已补查的查询词（同一词只补一次，换词重新算）。</summary>
    private string? PartialRequeryQuery;
    /// <summary>partial 补查延迟：给 native 插件预热/慢响应留的窗口。</summary>
    private const int PartialRequeryDelayMs = 600;

    /// <summary>结果落地：id 序列没变就不动集合（退格/微调零闪烁），变了才一次性
    /// 批量替换（单次 Reset，避免逐项 Add 触发 N 次布局）；旧对象按 id 复用保留
    /// 已加载的图标，新项先字母占位渲染，图标后台提取完成后补上。</summary>
    private void ApplyResults(QueryResultDto result, int gen, string q)
    {
        var sameIds = result.Items.Count == _items.Count;
        if (sameIds)
        {
            for (var i = 0; i < result.Items.Count; i++)
            {
                if (!result.Items[i].Id.Equals(_items[i].Id, StringComparison.OrdinalIgnoreCase))
                {
                    sameIds = false;
                    break;
                }
            }
        }

        var existing = new Dictionary<string, CandidateDto>(StringComparer.OrdinalIgnoreCase);
        foreach (var x in _items) existing[x.Id] = x;

        var newItems = new List<CandidateDto>(result.Items.Count);
        for (var i = 0; i < result.Items.Count; i++)
        {
            var item = result.Items[i];
            if (existing.TryGetValue(item.Id, out var old))
            {
                // 复用旧对象：保留 IconImage（不重复解码/不闪），只同步可能变化的展示字段
                if (old.Title != item.Title) old.Title = item.Title;
                if (old.Subtitle != item.Subtitle) old.Subtitle = item.Subtitle;
                if (old.Source != item.Source) old.Source = item.Source;
                if (old.Target != item.Target) old.Target = item.Target;
                if (old.IconPath != item.IconPath) old.IconPath = item.IconPath;
                old.Shortcut = i < 9 ? $"{i + 1}" : "";
                newItems.Add(old);
            }
            else
            {
                item.Shortcut = i < 9 ? $"{i + 1}" : "";
                newItems.Add(item);
            }
            // 图标未加载（新项 / 之前失败）→ 字母占位 + 后台补齐
            if (newItems[^1].IconImage is null)
                _ = LoadIconAsync(newItems[^1], gen);
        }

        // 高亮查询词同步到每个结果项（复用对象也要更新，容器 DataContextChanged
        // 只在引用变化时触发，同项复用依赖这里主动刷新）
        foreach (var x in newItems) x.HighlightQuery = q;

        if (!sameIds)
        {
            _items.ReplaceAll(newItems);
            _active = 0;
            ResultList.SelectedIndex = 0;
            ResultGrid.SelectedIndex = 0;
            // 输入搜索后焦点回到结果区，收藏选中态复位
            _favActive = -1;
            UpdateFavCardStates();
        }

        SearchMeta.Text = _items.Count > 0 ? $"{_items.Count} 项" : "";
        Footer.Text = _host.IsConnected ? "Host · 极速" : "演示 · 本地";
        // 空状态提示：搜索无结果 / 无最近使用时居中展示，避免页面空洞
        if (_items.Count > 0)
        {
            EmptyState.Visibility = Visibility.Collapsed;
        }
        else
        {
            EmptyState.Visibility = Visibility.Visible;
            EmptyState.Text = string.IsNullOrWhiteSpace(q)
                ? "还没有最近使用记录"
                : $"未找到「{q.Trim()}」相关结果";
        }
        // 搜索时收藏区变淡（对齐原型 dimmed）
        FavRoot.Opacity = string.IsNullOrWhiteSpace(q) ? 1.0 : 0.45;

        // 高亮刷新：容器复用同一 item 时 DataContextChanged 不触发，这里主动重建
        RefreshResultHighlights();
    }

    // ==================== 标题匹配高亮 ====================

    /// <summary>结果项 DataContext 变化时渲染标题（容器生成/复用时触发；
    /// 同项复用由 RefreshResultHighlights 兜底）。
    /// 不用 Text 绑定 + Inlines 互斥：WinUI 3 中 Inlines 集合一旦被访问，
    /// Text 绑定就会被忽略（即使 Inlines 为空）——统一由代码渲染文本。</summary>
    private void OnTitleDataContextChanged(object sender, DataContextChangedEventArgs e)
    {
        if (sender is TextBlock tb && tb.DataContext is CandidateDto item)
        {
            RenderTitle(tb, item);
        }
    }

    /// <summary>渲染标题文本：无高亮（空查询/无匹配）设 Text，有高亮填 Runs。</summary>
    private void RenderTitle(TextBlock tb, CandidateDto item)
    {
        var q = item.HighlightQuery;
        if (string.IsNullOrEmpty(q) || string.IsNullOrEmpty(item.Title))
        {
            if (tb.Inlines.Count > 0)
                tb.Inlines.Clear();
            tb.Text = item.Title;
            return;
        }
        tb.Text = "";
        tb.Inlines.Clear();
        var segs = FindHighlightSegments(item.Title, q);
        if (segs.Count == 0)
        {
            tb.Text = item.Title;
            return;
        }

        var accent = (Brush)Root.Resources["AccentBrush"];
        var title = item.Title;
        var pos = 0;
        foreach (var (start, len) in segs)
        {
            if (start > pos)
                tb.Inlines.Add(new Run { Text = title.Substring(pos, start - pos) });
            tb.Inlines.Add(new Run { Text = title.Substring(start, len), Foreground = accent });
            pos = start + len;
        }
        if (pos < title.Length)
            tb.Inlines.Add(new Run { Text = title.Substring(pos) });
    }

    /// <summary>标题高亮分段：先找 query 的连续子串；找不到再逐字符顺序匹配
    /// （近似 host 的模糊搜索，相邻命中合并成段）。不区分大小写。</summary>
    private static List<(int Start, int Len)> FindHighlightSegments(string title, string query)
    {
        var segs = new List<(int Start, int Len)>();
        var t = title.ToLowerInvariant();
        var q = query.ToLowerInvariant();

        var idx = t.IndexOf(q, StringComparison.Ordinal);
        if (idx >= 0)
        {
            segs.Add((idx, q.Length));
            return segs;
        }

        var ti = 0;
        foreach (var ch in q)
        {
            var found = t.IndexOf(ch, ti);
            if (found < 0) break;
            if (segs.Count > 0 && segs[^1].Start + segs[^1].Len == found)
                segs[^1] = (segs[^1].Start, segs[^1].Len + 1);  // 与上段相邻，合并
            else
                segs.Add((found, 1));
            ti = found + 1;
        }
        return segs;
    }

    /// <summary>主动重建所有已生成容器的标题高亮（虚拟化未生成的由
    /// OnTitleDataContextChanged 兜底）。</summary>
    private void RefreshResultHighlights()
    {
        for (var i = 0; i < _items.Count; i++)
        {
            if (ResultList.ContainerFromIndex(i) is ListViewItem lvi
                && (lvi.ContentTemplateRoot as FrameworkElement)?.FindName("RowTitle") is TextBlock rowTb
                && rowTb.DataContext is CandidateDto rowItem)
                RenderTitle(rowTb, rowItem);

            if (ResultGrid.ContainerFromIndex(i) is GridViewItem gvi
                && (gvi.ContentTemplateRoot as FrameworkElement)?.FindName("TileTitle") is TextBlock tileTb
                && tileTb.DataContext is CandidateDto tileItem)
                RenderTitle(tileTb, tileItem);
        }
    }

    /// <summary>插件候选判别：host 正常候选 Source=="plugin"；收藏兜底重建的 CandidateDto
    /// 可能丢 Source（FavEntryDto 快照不含该字段），用 PluginId/Target 前缀防御性补判。</summary>
    private static bool IsPluginCandidate(CandidateDto c)
        => c.Source == "plugin"
           || c.PluginId is not null
           || (c.Target?.StartsWith("plugin:", StringComparison.Ordinal) ?? false);

    /// <summary>后台取图标补到行上：gen 过期（新一轮查询已开始）则丢弃。
    /// 插件候选的图标是本地图片文件（svg/png），走 PluginIconLoader（与设置页同款）；
    /// 其余候选是 exe/dll/lnk 的 GDI 图标提取，走 AppIconService。</summary>
    private async Task LoadIconAsync(CandidateDto item, int gen)
    {
        try
        {
            ImageSource? src;
            if (IsPluginCandidate(item))
            {
                // ImageSource 有线程亲和性，构造必须在 UI 线程；磁盘嗅探在 LoadAsync 内部走后台线程
                src = await PluginIconLoader.LoadAsync(item.IconPath);
            }
            else
            {
                src = await AppIconService.GetIconAsync(item.Id, item.Target ?? item.IconPath);
            }
            if (src is null || gen != _queryGen) return;
            item.IconImage = src;
        }
        catch (Exception ex)
        {
            App.Log("LoadIcon", ex);
        }
    }

    /// <summary>收藏卡图标异步补齐：后台提取完成后替换字母占位。
    /// 卡片可能已被重建（切组/重绘），对孤立元素赋值无害。
    /// 插件候选走 PluginIconLoader 读本地图片文件，其余走 AppIconService GDI 提取。</summary>
    private async Task LoadFavIconAsync(Image img, Border letter, CandidateDto c)
    {
        try
        {
            ImageSource? src;
            if (IsPluginCandidate(c))
            {
                // ImageSource 有线程亲和性，构造必须在 UI 线程；磁盘嗅探在 LoadAsync 内部走后台线程
                src = await PluginIconLoader.LoadAsync(c.IconPath);
            }
            else
            {
                src = await AppIconService.GetIconAsync(c.Id, c.Target ?? c.IconPath);
            }
            if (src is null) return;
            img.Source = src;
            img.Visibility = Visibility.Visible;
            letter.Visibility = Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            App.Log("FavIcon", ex);
        }
    }

    // ==================== 收藏坞 ====================

    private void RenderFavorites()
    {
        // 记录当前主体实际高度：换内容后自然高度会变化（空↔有项约 12px），平滑过渡而非跳变
        var oldBodyH = FavBodyClip.Visibility == Visibility.Visible ? FavBodyClip.ActualHeight : 0;
        FavGroups.Children.Clear();
        FavItems.Children.Clear();
        _favActive = -1;  // 收藏内容重建后选中态复位
        var fav = LocalState.Fav;
        var res = Root.Resources;
        FavCount.Text = fav.Items.Count > 0 ? $"({fav.Items.Count})" : "";

        // 分组 tabs（对齐原型 .fav-group-tab）
        foreach (var g in fav.Groups)
        {
            var active = g.Id == fav.ActiveGroup;
            var btn = new Button
            {
                Content = g.Name,
                FontSize = 11,
                // 选中态不加粗：加粗中文笔画会变矮/挤压（如"工"），且 pill 宽度随选中跳动；
                // 用背景/边框/前景色区分即可
                FontWeight = FontWeights.Medium,
                Padding = new Thickness(10, 4, 10, 4),
                CornerRadius = new CornerRadius(999),
                BorderThickness = new Thickness(1),
                Background = active ? (Brush)res["AccentSoftBrush"] : new SolidColorBrush(Colors.Transparent),
                BorderBrush = active ? (Brush)res["RowActiveBrush"] : new SolidColorBrush(Colors.Transparent),
                Foreground = active ? (Brush)res["TextPrimaryBrush"] : (Brush)res["TextTertiaryBrush"],
            };
            var gid = g.Id;
            btn.Click += (_, _) => SwitchFavGroup(gid);
            // 右键分组 tab：删除分组（「全部」不可删，它是兜底容器）
            if (gid != "all")
                btn.RightTapped += (_, e) => ShowFavGroupMenu(btn, e, gid, g.Name);
            FavGroups.Children.Add(btn);
        }

        // 收藏项（按分组过滤；真实收藏可增删，不再给无法移除的演示项）
        var entries = fav.Items
            .Where(x => fav.ActiveGroup == "all" || x.GroupId == fav.ActiveGroup)
            .ToList();
        var ids = entries.Select(x => x.ItemId).Distinct().ToList();

        if (ids.Count == 0)
        {
            FavItems.Children.Add(new TextBlock
            {
                Text = "（该分组暂无收藏）",
                FontSize = 11,
                Foreground = (Brush)res["TextTertiaryBrush"],
                VerticalAlignment = VerticalAlignment.Center,
            });
        }

        foreach (var id in ids)
        {
            var entry = entries.FirstOrDefault(x => x.ItemId == id);
            // 优先当前搜索结果/演示数据，其次用收藏时快照的元数据渲染（不在结果里也能显示卡片）
            var c = _items.FirstOrDefault(x => x.Id == id) ?? DemoData.Find(id);
            if (c is null && entry is not null && !string.IsNullOrEmpty(entry.Title))
                c = new CandidateDto { Id = id, Title = entry.Title, Target = entry.Target, IconPath = entry.IconPath };
            if (c is null) continue;
            var title = entry?.Title ?? c.Title;

            // 图标：字母占位立即渲染，真实图标后台提取完成后替换（避免 GDI 阻塞 UI 线程）
            var iconEl = new Grid
            {
                Width = 36, Height = 36,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center
            };
            var letterTile = new Border
            {
                CornerRadius = new CornerRadius(10),
                Background = c.IconBrush,
                Child = new TextBlock
                {
                    Text = c.IconGlyph, FontSize = 12, FontWeight = FontWeights.SemiBold,
                    Foreground = new SolidColorBrush(Colors.White),
                    HorizontalAlignment = HorizontalAlignment.Center,
                    VerticalAlignment = VerticalAlignment.Center
                }
            };
            var iconImg = new Image
            {
                Width = 36, Height = 36, Stretch = Stretch.Uniform,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
                Visibility = Visibility.Collapsed
            };
            iconEl.Children.Add(letterTile);
            iconEl.Children.Add(iconImg);
            _ = LoadFavIconAsync(iconImg, letterTile, c);

            var panel = new StackPanel
            {
                Width = 72, Spacing = 6, Padding = new Thickness(6, 8, 6, 8)
            };
            panel.Children.Add(iconEl);
            panel.Children.Add(new TextBlock
            {
                Text = title, FontSize = 10, Foreground = (Brush)res["TextSecondaryBrush"],
                TextAlignment = TextAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis,
                MaxLines = 1
            });

            var btn = new Button
            {
                Content = panel,
                Padding = new Thickness(0),
                Background = new SolidColorBrush(Colors.Transparent),
                BorderThickness = new Thickness(1),
                BorderBrush = new SolidColorBrush(Colors.Transparent),
                UseSystemFocusVisuals = false,
                Template = (ControlTemplate)Root.Resources["FavCardTemplate"],
                Tag = c.Id
            };
            // 无默认边框/底色；悬停与键盘选中（光标）复用上方平铺卡片的白色选中样式
            btn.PointerEntered += (_, _) =>
            {
                var res = Root.Resources;
                btn.Background = (Brush)res["GridTileSelBgBrush"];
                btn.BorderBrush = (Brush)res["GridTileSelBorderBrush"];
            };
            btn.PointerExited += (_, _) => UpdateFavCardStates();
            ToolTipService.SetToolTip(btn, title);
            var itemId = c.Id;
            btn.Click += async (_, _) =>
            {
                try
                {
                    await _host.InvokeAsync(itemId, "open", QueryBox.Text ?? "");
                    Footer.Text = "已执行：" + title;
                }
                catch (Exception ex) { App.Log("FavInvoke", ex); Footer.Text = "执行失败：" + title; }
                // 执行后隐藏是固化默认行为（设置页不提供开关）
                HideLauncher();
            };
            // 右键收藏卡片：取消收藏
            btn.RightTapped += (_, e) => ShowFavCardMenu(btn, e, itemId, title);
            FavItems.Children.Add(btn);
        }

        // 折叠状态由 ApplyFavCollapse 管（带动画），这里只同步提示文字
        ToolTipService.SetToolTip(FavToggle, fav.Expanded ? "收起收藏" : "展开收藏");
        UpdateFavCardStates();
        AnimateFavBodyHeight(oldBodyH);
        SyncFavClip();
    }

    /// <summary>同步裁剪框到当前外层高度（静止态：外层为自然高度）。
    /// 不设的话 RectangleGeometry 默认空矩形，内容会被裁掉看不见。</summary>
    private void SyncFavClip()
    {
        var h = double.IsNaN(FavBodyClip.Height) ? FavBody.ActualHeight : FavBodyClip.Height;
        FavBodyClipRect.Rect = new Rect(0, 0, FavBodyClip.ActualWidth, h > 0 ? h : 0);
    }

    /// <summary>
    /// 收藏内容高度过渡：分组切换/增删后主体自然高度变化（空↔有项），从当前实际高度平滑收放到新高度。
    /// 复用抽屉的渲染帧驱动；折叠动画进行中或折叠态时跳过（由 ApplyFavCollapse 接管）。
    /// </summary>
    private void AnimateFavBodyHeight(double h0)
    {
        // 窗口未上屏（启动首帧）：直接落定自然高度，不播过渡
        if (!_visible) return;
        if (FavBodyClip.Visibility != Visibility.Visible) return;
        // 抽屉动画进行中交给它；仅当正在跑"纯高度过渡"时可中途续动（连续快速切分组）
        if (_favTweening && !_favHeightOnly) return;
        if (h0 <= 0) return;

        // 内容已换好：先量出新自然高度（此刻尚未渲染，量完压回旧高度起动画，不闪一帧新高度）
        FavBody.Height = double.NaN;
        FavBodyClip.Height = double.NaN;
        FavBodyClip.UpdateLayout();
        var h1 = FavBody.ActualHeight;
        if (h1 <= 0 || Math.Abs(h1 - h0) < 0.5)
        {
            FavBody.Height = double.NaN;
            FavBodyClip.Height = double.NaN;
            return;
        }
        // 内容层固定到新自然高度：动画期间外层 Border 高度变化不会触发内容重测
        FavBody.Height = h1;
        FavBodyClip.Height = h0;
        FavBodyClip.UpdateLayout();

        _favTweening = true;
        _favAnimGen++;
        _favCollapsing = false;
        _favHeightOnly = true;
        _favH0 = h0;
        _favH1 = h1;
        _favO0 = FavBodyClip.Opacity;
        _favG0 = FavGroups.Opacity;
        _favA0 = FavChevronRotate.Angle;
        _favS0 = FavChevronShift.Y;
        _favTweenStart = Environment.TickCount64;
        // 高度差很小（约 12px）：120ms + 1.5 次方曲线（与收起同款），干脆不拖尾
        _favTweenMs = 120;
    }

    // ==================== 分组切换过渡 ====================

    /// <summary>切换到指定分组。带过渡：旧内容淡出 → 重建 → 新内容淡入 + 卡片逐项 pop；
    /// 窗口未上屏时直接重建（与折叠/高度过渡同一策略）。</summary>
    private void SwitchFavGroup(string gid)
    {
        if (LocalState.Fav.ActiveGroup == gid) return;
        LocalState.Fav.ActiveGroup = gid;
        LocalState.SaveFav();
        if (!_visible)
        {
            RenderFavorites();
            return;
        }
        FavSwitchStart();
    }

    /// <summary>启动淡出阶段：从当前视觉状态续动（若上一段过渡未完成，快速连续切组不跳变）。</summary>
    private void FavSwitchStart()
    {
        _favSwitching = true;
        _favSwitchOut = true;
        _favSwitchO0 = FavItems.Opacity;
        _favSwitchY0 = FavItemsShift.Y;
        _favSwitchPhaseStart = Environment.TickCount64;
        // 淡出 150ms（对齐原型 is-leaving 的 160ms），ease-in 收尾自然
        _favSwitchPhaseMs = 150;
        // 过渡期间旧卡片不可点，避免 hover 选中态闪动
        FavItems.IsHitTestVisible = false;
    }

    /// <summary>每渲染帧驱动切换：淡出阶段结束原地换内容；入场阶段容器 + 逐项 pop，全部完成落定。</summary>
    private void UpdateFavSwitch()
    {
        var now = Environment.TickCount64;
        var t = (now - _favSwitchPhaseStart) / (double)_favSwitchPhaseMs;
        var done = t >= 1;
        if (done) t = 1;

        if (_favSwitchOut)
        {
            // 旧内容淡出并下移 6px（ease-in）
            var k = t * t;
            FavItems.Opacity = _favSwitchO0 + (0 - _favSwitchO0) * k;
            FavItemsShift.Y = _favSwitchY0 + (6 - _favSwitchY0) * k;
            if (!done) return;

            // 淡出完成：原地换内容（重建会同步 tab 选中态与主体高度过渡），随后开入场阶段
            _favSwitchOut = false;
            RenderFavorites();
            _favSwitchItems.Clear();
            foreach (var b in FavItems.Children.OfType<Button>().ToList())
            {
                b.RenderTransformOrigin = new Point(0.5, 0.5);
                var shift = new TranslateTransform();
                var scale = new ScaleTransform();
                b.RenderTransform = new TransformGroup { Children = { shift, scale } };
                b.Opacity = 0;
                _favSwitchItems.Add((b, shift, scale));
            }
            _favSwitchPhaseStart = now;
            _favSwitchPhaseMs = 280;
            _favSwitchO0 = 0;
            _favSwitchY0 = 8;
            FavItems.Opacity = 0;
            FavItemsShift.Y = 8;
            FavItems.IsHitTestVisible = true;
            return;
        }

        // 入场：容器淡入上移 8px（二次缓出，对齐原型 fav-enter）；卡片逐项 pop
        // （每项延迟 30ms、300ms 二次缓出：透明度 + 下移 8px + 0.96→1 缩放）
        var ck = 1 - (1 - t) * (1 - t);
        FavItems.Opacity = _favSwitchO0 + (1 - _favSwitchO0) * ck;
        FavItemsShift.Y = _favSwitchY0 + (0 - _favSwitchY0) * ck;
        var allDone = done;
        for (var i = 0; i < _favSwitchItems.Count; i++)
        {
            var ti = (now - _favSwitchPhaseStart - i * 30.0) / 300.0;
            var (b, shift, scale) = _favSwitchItems[i];
            if (ti <= 0) { allDone = false; continue; }
            if (ti >= 1) { ti = 1; } else { allDone = false; }
            var ik = 1 - (1 - ti) * (1 - ti);
            b.Opacity = ik;
            shift.Y = (1 - ik) * 8;
            scale.ScaleX = scale.ScaleY = 0.96 + 0.04 * ik;
        }
        if (allDone) FavSwitchSettle();
    }

    /// <summary>落定切换：容器与卡片还原，清空捕获的变换（下次切换重新挂）。</summary>
    private void FavSwitchSettle()
    {
        _favSwitching = false;
        FavItems.IsHitTestVisible = true;
        FavItems.Opacity = 1;
        FavItemsShift.Y = 0;
        foreach (var (b, _, _) in _favSwitchItems)
        {
            b.Opacity = 1;
            b.RenderTransform = null;
        }
        _favSwitchItems.Clear();
    }

    /// <summary>中断切换（折叠/增删收藏等外部重建）：还原视觉状态；
    /// 淡出阶段内容还是旧分组且 rebuild=true 时先重建落定，避免重新展开后显示旧内容。</summary>
    private void FavSwitchCancel(bool rebuild)
    {
        if (!_favSwitching) return;
        _favSwitching = false;
        if (rebuild && _favSwitchOut) RenderFavorites();
        FavItems.IsHitTestVisible = true;
        FavItems.Opacity = 1;
        FavItemsShift.Y = 0;
        foreach (var (b, _, _) in _favSwitchItems)
        {
            b.Opacity = 1;
            b.RenderTransform = null;
        }
        _favSwitchItems.Clear();
    }

    // ==================== 右键菜单 / 收藏增删 ====================

    /// <summary>右键搜索结果（列表/平铺共用）：定位到命中项，在指针位置弹动作菜单。</summary>
    private void OnResultRightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        var node = e.OriginalSource as DependencyObject;
        while (node is not null and not ListViewItem and not GridViewItem)
            node = VisualTreeHelper.GetParent(node);
        // 注意：自定义 ControlTemplate 下容器 DataContext 为 null，取 Content（= 数据项）
        if (node is not ContentControl cc || cc.Content is not CandidateDto c) return;
        _active = _items.IndexOf(c);
        _favActive = -1;  // 右键结果区 = 焦点回结果区，收藏选中态复位
        SyncSelection();
        _itemMenu = BuildItemMenu(c);
        _itemMenu.ShowAt(cc, new FlyoutShowOptions { Position = e.GetPosition(cc) });
    }

    /// <summary>构建搜索结果动作菜单：打开 / 管理员 / 打开位置 / 收藏（已收藏则取消收藏）。</summary>
    private MenuFlyout BuildItemMenu(CandidateDto c)
    {
        var menu = new MenuFlyout();
        var itemId = c.Id;

        var open = new MenuFlyoutItem { Text = "打开", Icon = new FontIcon { Glyph = "\uE8A7" } };
        open.Click += (_, _) => _ = InvokeActionAsync(itemId, "open");
        menu.Items.Add(open);

        // 附属快捷方式动作（如"Chrome 无痕模式"）：同一应用合并后的其他打开方式
        var altActions = c.Actions
            .Where(a => !a.IsDefault
                        && a.Id is not ("open" or "runas" or "reveal")
                        && !string.IsNullOrWhiteSpace(a.Title))
            .ToList();
        if (altActions.Count > 0)
        {
            var sub = new MenuFlyoutSubItem { Text = "打开方式", Icon = new FontIcon { Glyph = "\uE8A7" } };
            foreach (var a in altActions)
            {
                var aid = a.Id;
                var mi = new MenuFlyoutItem { Text = a.Title };
                mi.Click += (_, _) => _ = InvokeActionAsync(itemId, aid);
                sub.Items.Add(mi);
            }
            menu.Items.Add(sub);
        }

        var runas = new MenuFlyoutItem { Text = "以管理员身份打开", Icon = new FontIcon { Glyph = "\uE7EF" } };
        runas.Click += (_, _) => _ = InvokeActionAsync(itemId, "runas");
        menu.Items.Add(runas);

        var reveal = new MenuFlyoutItem { Text = "打开文件位置", Icon = new FontIcon { Glyph = "\uE838" } };
        reveal.Click += (_, _) => _ = InvokeActionAsync(itemId, "reveal");
        menu.Items.Add(reveal);

        menu.Items.Add(new MenuFlyoutSeparator());

        var fav = LocalState.Fav;
        if (fav.Items.Any(x => x.ItemId == itemId))
        {
            var unpin = new MenuFlyoutItem { Text = "取消收藏", Icon = new FontIcon { Glyph = "\uE74D" } };
            unpin.Click += (_, _) => RemoveFavorite(itemId);
            menu.Items.Add(unpin);
        }
        else
        {
            // 「全部」是各分组的汇总视图，不是可收藏的分组：只列真实分组；
            // 没有分组时（新装默认只有「全部」）也有「新建分组…」兜底，建完自动收藏
            var sub = new MenuFlyoutSubItem { Text = "收藏到", Icon = new FontIcon { Glyph = "\uE734" } };
            foreach (var g in fav.Groups.Where(g => g.Id != "all"))
            {
                var gid = g.Id;
                var gi = new MenuFlyoutItem { Text = g.Name };
                gi.Click += (_, _) => AddFavorite(c, gid);
                sub.Items.Add(gi);
            }
            if (sub.Items.Count > 0) sub.Items.Add(new MenuFlyoutSeparator());
            var create = new MenuFlyoutItem { Text = "新建分组…", Icon = new FontIcon { Glyph = "\uE710" } };
            create.Click += (_, _) =>
            {
                _pendingFavItem = c;
                ShowFavGroupPanel();
            };
            sub.Items.Add(create);
            menu.Items.Add(sub);
        }
        return menu;
    }

    /// <summary>Tab 动作：对当前选中项弹出动作菜单（对齐原型 action-sheet）。</summary>
    private void ShowActiveItemMenu()
    {
        if (_items.Count == 0 || _active < 0 || _active >= _items.Count) return;
        var c = _items[_active];
        var anchor = _gridView
            ? ResultGrid.ContainerFromIndex(_active) as FrameworkElement
            : ResultList.ContainerFromIndex(_active) as FrameworkElement;
        // 容器未实现（离屏）：退回在列表/网格根上弹出
        anchor ??= _gridView ? ResultGrid : ResultList;
        _itemMenu = BuildItemMenu(c);
        _itemMenu.ShowAt(anchor, new FlyoutShowOptions { Placement = FlyoutPlacementMode.BottomEdgeAlignedLeft });
    }

    /// <summary>右键收藏卡片：打开 / 取消收藏。</summary>
    private void ShowFavCardMenu(FrameworkElement anchor, RightTappedRoutedEventArgs e, string itemId, string title)
    {
        var menu = new MenuFlyout();
        var open = new MenuFlyoutItem { Text = "打开" };
        open.Click += (_, _) => _ = InvokeActionAsync(itemId, "open", title);
        menu.Items.Add(open);
        menu.Items.Add(new MenuFlyoutSeparator());
        var rm = new MenuFlyoutItem { Text = "取消收藏" };
        rm.Click += (_, _) => RemoveFavorite(itemId);
        menu.Items.Add(rm);
        menu.ShowAt(anchor, new FlyoutShowOptions { Position = e.GetPosition(anchor) });
    }

    /// <summary>收藏当前项到指定分组；已在收藏则移动分组（对齐原型 pinToGroup）。</summary>
    private void AddFavorite(CandidateDto c, string groupId)
    {
        var fav = LocalState.Fav;
        // 打断进行中的分组切换（下面直接重建，内容即时落定）
        FavSwitchCancel(rebuild: false);
        var existing = fav.Items.FirstOrDefault(x => x.ItemId == c.Id);
        if (existing is not null)
        {
            existing.GroupId = groupId;
            existing.Title ??= c.Title;
            existing.Target ??= c.Target;
            existing.IconPath ??= c.IconPath;
        }
        else
        {
            fav.Items.Add(new FavEntryDto
            {
                ItemId = c.Id,
                GroupId = groupId,
                Title = c.Title,
                Target = c.Target,
                IconPath = c.IconPath,
            });
        }
        var gname = fav.Groups.FirstOrDefault(g => g.Id == groupId)?.Name ?? groupId;
        LocalState.SaveFav();
        RenderFavorites();
        ApplyFavCollapse(false, animate: true);
        Footer.Text = $"已收藏到「{gname}」";
    }

    /// <summary>从收藏中移除（全部分组）。</summary>
    private void RemoveFavorite(string itemId)
    {
        var fav = LocalState.Fav;
        if (fav.Items.RemoveAll(x => x.ItemId == itemId) == 0) return;
        // 打断进行中的分组切换（下面直接重建，内容即时落定）
        FavSwitchCancel(rebuild: false);
        LocalState.SaveFav();
        RenderFavorites();
        Footer.Text = "已取消收藏";
    }

    /// <summary>
    /// 收藏坞折叠态：像抽屉一样推回/拉出——主体高度逐帧伸缩 + 淡入淡出 + 箭头旋转 + 分组行淡出
    /// （对齐原型 .favorites.is-collapsed 过渡）。animate=false 时直接落定（启动、新建分组、减少动画）。
    /// </summary>
    private void ApplyFavCollapse(bool collapsed, bool animate)
    {
        // 打断进行中的分组切换：淡出阶段内容仍是旧分组，先重建落定（折叠后重新展开显示正确分组）
        FavSwitchCancel(rebuild: true);
        _favTweening = false;
        _favAnimGen++;
        _favHeightOnly = false;
        var gen = _favAnimGen;

        if (!animate)
        {
            FavSettle(collapsed, gen);
            return;
        }

        double h;
        if (collapsed)
        {
            // 折叠：从当前实际高度开始收（此刻即自然高度，首帧无跳变）
            h = FavBody.ActualHeight > 0 ? FavBody.ActualHeight : 76;
            _favGroupsW0 = FavGroupsCol.ActualWidth > 0 ? FavGroupsCol.ActualWidth : 300;
        }
        else
        {
            // 展开：先恢复列宽/可点（内容透明瞬间看不出跳变），量出内容自然高度
            FavGroupsCol.Width = new GridLength(1, GridUnitType.Star);
            FavAddGroup.Width = 22;
            FavGroups.IsHitTestVisible = FavAddGroup.IsHitTestVisible = true;
            FavBodyClip.Visibility = Visibility.Visible;
            FavBody.Height = double.NaN;
            FavBodyClip.Height = double.NaN;
            FavBodyClip.UpdateLayout();
            h = FavBodyClip.ActualHeight > 0 ? FavBodyClip.ActualHeight : 76;
            _favGroupsW0 = FavGroupsCol.ActualWidth > 0 ? FavGroupsCol.ActualWidth : 300;
            // 量完立即压回 0 并应用布局：否则 Timer 首帧前会按自然高度闪出一帧完整抽屉
            // （展开时"弹一下"的来源：完整展开一帧 → 瞬间缩回 0 → 再平滑拉出）
            FavBody.Height = h;
            FavBodyClip.Height = 0;
            FavBodyClip.UpdateLayout();
        }

        // 内容层固定自然高度：动画期间外层 Border 高度变化不再重测内容（流畅的关键）
        FavBody.Height = h;

        // 起点取当前实际值，连续快速切换可从中途续动
        _favCollapsing = collapsed;
        _favH0 = collapsed ? h : 0;
        _favH1 = collapsed ? 0 : h;
        _favO0 = FavBodyClip.Opacity;
        _favG0 = FavGroups.Opacity;
        _favA0 = FavChevronRotate.Angle;
        _favS0 = FavChevronShift.Y;
        _favTweenStart = Environment.TickCount64;
        // 展开/收起统一 150ms，干脆利落（收起 1.5 次方曲线下前后更均匀）
        _favTweenMs = 150;
        FavBodyClip.Visibility = Visibility.Visible;
        _favTweening = true;
    }

    /// <summary>每渲染帧：按缓动曲线插值高度/透明度/箭头/分组行，结束后落定。</summary>
    private void OnFavRendering(object? sender, object e)
    {
        if (_favSwitching) UpdateFavSwitch();
        if (!_favTweening) return;
        var t = (Environment.TickCount64 - _favTweenStart) / (double)_favTweenMs;
        var done = t >= 1;
        if (done) t = 1;
        // 缓动：收起/高度过渡走 1-(1-t)^1.5——速度从 1.5 线性降到 0，比 quadratic（2→0）前后段更均匀、
        // 不拖尾（quadratic 后段明显偏慢，感知"前后不和谐"）；展开保持 quadratic（轻快）
        var k = (_favCollapsing || _favHeightOnly)
            ? 1 - Math.Pow(1 - t, 1.5)
            : 1 - Math.Pow(1 - t, 2);

        FavBodyClip.Height = _favH0 + (_favH1 - _favH0) * k;
        FavBodyClip.Opacity = _favO0 + ((_favCollapsing ? 0 : 1) - _favO0) * k;
        // 裁剪框跟随外层高度（Clip 是视觉属性，不触发布局；高度动画只此一处布局）
        FavBodyClipRect.Rect = new Rect(0, 0, FavBodyClip.ActualWidth, FavBodyClip.Height);
        FavChevronRotate.Angle = _favA0 + ((_favCollapsing ? -90 : 0) - _favA0) * k;
        // 收起后箭头旋转成竖长 ">"：视觉重心比星星/文字高约 2px，随动画下移对齐（展开恢复）
        FavChevronShift.Y = _favS0 + ((_favCollapsing ? 2 : 0) - _favS0) * k;
        FavGroups.Opacity = _favG0 + ((_favCollapsing ? 0 : 1) - _favG0) * k;
        FavAddGroup.Opacity = _favG0 + ((_favCollapsing ? 0 : 1) - _favG0) * k;
        // 布局型属性（间距/分组列宽/按钮宽）只在前半程收放，后半程保持终值——
        // 避免尾部多布局属性叠加（ScrollViewer 裁剪 + 列宽重排）导致单帧超时、动画"直接跳到底"；
        // 纯主体高度过渡（内容空↔有项）不涉及这些属性，跳过以免首帧把它们压成 0
        if (!_favHeightOnly)
        {
            var p = _favCollapsing ? 1 - Math.Min(1, (1 - k) * 2) : Math.Min(1, k * 2);
            FavRoot.Spacing = 8 * p;
            FavGroupsCol.Width = new GridLength(_favGroupsW0 * p);
            FavAddGroup.Width = 22 * p;
        }

        if (done)
        {
            _favTweening = false;
            FavSettle(_favCollapsing, _favAnimGen);
        }
    }

    /// <summary>落定收藏坞折叠态（动画结束 / 减少动画 / 启动）。</summary>
    private void FavSettle(bool collapsed, int gen)
    {
        if (gen != _favAnimGen) return;
        FavBodyClip.Visibility = collapsed ? Visibility.Collapsed : Visibility.Visible;
        FavBody.Height = double.NaN;
        FavBodyClip.Height = double.NaN;
        FavBodyClip.Opacity = 1;
        FavRoot.Spacing = collapsed ? 0 : 8;
        FavGroupsCol.Width = collapsed ? new GridLength(0) : new GridLength(1, GridUnitType.Star);
        FavAddGroup.Width = collapsed ? 0 : 22;
        FavGroups.Opacity = collapsed ? 0 : 1;
        FavAddGroup.Opacity = collapsed ? 0 : 1;
        FavGroups.IsHitTestVisible = FavAddGroup.IsHitTestVisible = !collapsed;
        FavChevronRotate.Angle = collapsed ? -90 : 0;
        FavChevronShift.Y = collapsed ? 2 : 0;
        SyncFavClip();
    }

    private void OnFavToggle(object sender, RoutedEventArgs e)
    {
        var fav = LocalState.Fav;
        fav.Expanded = !fav.Expanded;
        LocalState.SaveFav();
        RenderFavorites();
        ApplyFavCollapse(!fav.Expanded, animate: true);
    }

    // ==================== 新建分组（窗口内模态面板） ====================

    private void OnFavAddGroup(object sender, RoutedEventArgs e) => ShowFavGroupPanel();

    private void ShowFavGroupPanel()
    {
        FavGroupName.Text = "";
        FavGroupPanel.Visibility = Visibility.Visible;
        FavGroupName.Focus(FocusState.Programmatic);
        AnimateModalIn(FavGroupCard, FavGroupCardScale, FavGroupCardShift);
    }

    /// <summary>模态卡片入场：淡入 + 上浮放大（与窗口 pop-in 同款曲线）。</summary>
    private void AnimateModalIn(Border card, ScaleTransform scale, TranslateTransform shift)
    {
        // 起点即终值中间态，可被下次打开续动
        card.Opacity = 0;
        scale.ScaleX = scale.ScaleY = 0.96;
        shift.Y = 6;
        var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
        var d = new Duration(TimeSpan.FromMilliseconds(180));
        var sb = new Storyboard();
        void Add(DependencyObject target, string prop, double from, double to)
        {
            var a = new DoubleAnimation { From = from, To = to, Duration = d, EasingFunction = ease };
            Storyboard.SetTarget(a, target);
            Storyboard.SetTargetProperty(a, prop);
            sb.Children.Add(a);
        }
        Add(card, "Opacity", 0, 1);
        Add(scale, "ScaleX", 0.96, 1);
        Add(scale, "ScaleY", 0.96, 1);
        Add(shift, "Y", 6, 0);
        sb.Begin();
    }

    private void CloseFavGroupPanel()
    {
        FavGroupPanel.Visibility = Visibility.Collapsed;
        _pendingFavItem = null;   // 取消/关闭：丢弃待收藏项
        // 复位卡片视觉（被中断的入场动画可能停在中间态）
        FavGroupCard.Opacity = 1;
        FavGroupCardScale.ScaleX = FavGroupCardScale.ScaleY = 1;
        FavGroupCardShift.Y = 0;
        QueryBox.Focus(FocusState.Programmatic);
    }

    private void OnFavGroupNameKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != VirtualKey.Enter) return;
        e.Handled = true;
        CreateFavGroup();
    }

    private void OnFavGroupCreate(object sender, RoutedEventArgs e) => CreateFavGroup();

    private void OnFavGroupCancel(object sender, RoutedEventArgs e) => CloseFavGroupPanel();

    /// <summary>点面板外（遮罩）：取消。卡片内点击在 OnFavGroupCardTapped 里置 Handled，不会误关。</summary>
    private void OnFavGroupScrimTapped(object sender, TappedRoutedEventArgs e) => CloseFavGroupPanel();

    private void OnFavGroupCardTapped(object sender, TappedRoutedEventArgs e) => e.Handled = true;

    /// <summary>毫秒时间戳转 36 进制短 ID（对齐原型 Date.now().toString(36)）。
    /// 注意 Convert.ToString(long, toBase) 只支持 2/8/10/16，传 36 会抛 Invalid Base。</summary>
    private static string ToBase36(long v)
    {
        const string digits = "0123456789abcdefghijklmnopqrstuvwxyz";
        var s = "";
        do { s = digits[(int)(v % 36)] + s; v /= 36; } while (v > 0);
        return s;
    }

    private void CreateFavGroup()
    {
        var name = FavGroupName.Text.Trim();
        if (name.Length == 0) return;   // 空名称不创建（面板保持打开）
        if (name.Length > 8) name = name[..8];
        var id = "g_" + ToBase36(DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
        // 「收藏到 → 新建分组…」链路：建完直接把待收藏项收进新组
        var pending = _pendingFavItem;
        _pendingFavItem = null;
        LocalState.Fav.Groups.Add(new FavGroupDto { Id = id, Name = name });
        LocalState.Fav.ActiveGroup = id;
        LocalState.Fav.Expanded = true;
        LocalState.SaveFav();
        CloseFavGroupPanel();
        if (pending is not null)
        {
            AddFavorite(pending, id);   // 渲染 + 展开动画 + 「已收藏到」提示
            return;
        }
        RenderFavorites();
        ApplyFavCollapse(false, animate: false);
    }

    // ==================== 删除分组（右键 tab → 确认弹窗） ====================

    /// <summary>待删除的分组 Id（确认弹窗打开期间持有）。</summary>
    private string _favDeleteGroupId = "";
    /// <summary>「收藏到 → 新建分组…」待收藏的项；建组成功后自动收藏，取消则丢弃。</summary>
    private CandidateDto? _pendingFavItem;

    private void ShowFavGroupMenu(FrameworkElement anchor, RightTappedRoutedEventArgs e, string gid, string gname)
    {
        var menu = new MenuFlyout();
        var del = new MenuFlyoutItem { Text = "删除分组", Icon = new FontIcon { Glyph = "\uE74D" } };
        del.Click += (_, _) => ShowFavConfirmPanel(gid, gname);
        menu.Items.Add(del);
        menu.ShowAt(anchor, new FlyoutShowOptions { Position = e.GetPosition(anchor) });
    }

    private void ShowFavConfirmPanel(string gid, string gname)
    {
        _favDeleteGroupId = gid;
        var n = LocalState.Fav.Items.Count(x => x.GroupId == gid);
        FavConfirmMsg.Text = n > 0
            ? $"删除分组「{gname}」？组内 {n} 个收藏将一并删除。"
            : $"删除分组「{gname}」？";
        FavConfirmPanel.Visibility = Visibility.Visible;
        // 安全默认：焦点落在「取消」
        FavConfirmCancelBtn.Focus(FocusState.Programmatic);
        AnimateModalIn(FavConfirmCard, FavConfirmCardScale, FavConfirmCardShift);
    }

    private void CloseFavConfirmPanel()
    {
        FavConfirmPanel.Visibility = Visibility.Collapsed;
        _favDeleteGroupId = "";
        // 复位卡片视觉（被中断的入场动画可能停在中间态）
        FavConfirmCard.Opacity = 1;
        FavConfirmCardScale.ScaleX = FavConfirmCardScale.ScaleY = 1;
        FavConfirmCardShift.Y = 0;
        QueryBox.Focus(FocusState.Programmatic);
    }

    private void OnFavConfirmKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != VirtualKey.Enter) return;
        e.Handled = true;
        OnFavConfirmDelete(sender, e);
    }

    private void OnFavConfirmDelete(object sender, RoutedEventArgs e)
    {
        var fav = LocalState.Fav;
        var gid = _favDeleteGroupId;
        CloseFavConfirmPanel();
        if (gid == "all" || fav.Groups.RemoveAll(g => g.Id == gid) == 0) return;
        // 「全部」只是各分组的汇总视图：删分组连带删组内收藏，不留下无归属的孤儿项
        fav.Items.RemoveAll(x => x.GroupId == gid);
        // 删的是当前分组：退回「全部」
        if (fav.ActiveGroup == gid) fav.ActiveGroup = "all";
        LocalState.SaveFav();
        FavSwitchCancel(rebuild: false);  // 打断进行中的分组切换，内容即时落定
        RenderFavorites();
    }

    private void OnFavConfirmCancel(object sender, RoutedEventArgs e) => CloseFavConfirmPanel();

    /// <summary>点面板外（遮罩）：取消。卡片内点击在 OnFavConfirmCardTapped 里置 Handled，不会误关。</summary>
    private void OnFavConfirmScrimTapped(object sender, TappedRoutedEventArgs e) => CloseFavConfirmPanel();

    private void OnFavConfirmCardTapped(object sender, TappedRoutedEventArgs e) => e.Handled = true;

    // ==================== 视图切换 ====================

    private void SetView(bool grid)
    {
        if (_gridView == grid) return;
        _gridView = grid;
        var res = Root.Resources;
        var activeBg = (Brush)res["AccentSoftBrush"];
        var idleBg = new SolidColorBrush(Colors.Transparent);
        BtnViewList.Background = grid ? idleBg : activeBg;
        BtnViewGrid.Background = grid ? activeBg : idleBg;
        if (BtnViewList.Content is XamlPath pl)
            pl.Stroke = grid ? (Brush)res["TextTertiaryBrush"] : (Brush)res["TextPrimaryBrush"];
        if (BtnViewGrid.Content is XamlPath pg)
            pg.Fill = grid ? (Brush)res["TextPrimaryBrush"] : (Brush)res["TextTertiaryBrush"];

        // 列表 ↔ 平铺：出场淡出+轻微缩小下沉，入场放大浮现（对齐原型 .is-leaving / view-enter-*）
        var outgoing = grid ? (UIElement)ResultList : (UIElement)ResultGrid;
        var incoming = grid ? (UIElement)ResultGrid : (UIElement)ResultList;

        _viewAnim?.Stop();
        _viewAnimGen++;
        var gen = _viewAnimGen;

        // 上次动画可能中断在半途：两个面板同格叠放，先复位到可见不透明再播动画（Begin 立即套用 From 值，无闪烁）
        ResultList.Visibility = Visibility.Visible;
        ResultGrid.Visibility = Visibility.Visible;
        ResultList.Opacity = 1;
        ResultGrid.Opacity = 1;

        var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
        var inDur = new Duration(TimeSpan.FromMilliseconds(280));
        var outDur = new Duration(TimeSpan.FromMilliseconds(220));
        var sb = new Storyboard();
        void Add(DependencyObject target, string prop, double from, double to, Duration d)
        {
            var a = new DoubleAnimation { From = from, To = to, Duration = d, EasingFunction = ease };
            Storyboard.SetTarget(a, target);
            Storyboard.SetTargetProperty(a, prop);
            sb.Children.Add(a);
        }

        // 出场：淡出 + 缩到 0.97 + 下沉 6px（对齐原型 .results.is-leaving）
        var outT = new CompositeTransform();
        outgoing.RenderTransform = outT;
        Add(outgoing, "Opacity", 1, 0, outDur);
        Add(outT, "ScaleX", 1, 0.97, outDur);
        Add(outT, "ScaleY", 1, 0.97, outDur);
        Add(outT, "TranslateY", 0, 6, outDur);

        // 入场：平铺从下方 10px 放大浮现（scale 0.94），列表从左侧 -12px 滑入（scale 0.98）
        var inT = grid
            ? new CompositeTransform { ScaleX = 0.94, ScaleY = 0.94, TranslateY = 10 }
            : new CompositeTransform { ScaleX = 0.98, ScaleY = 0.98, TranslateX = -12 };
        incoming.RenderTransform = inT;
        Add(incoming, "Opacity", 0, 1, inDur);
        Add(inT, "ScaleX", inT.ScaleX, 1, inDur);
        Add(inT, "ScaleY", inT.ScaleY, 1, inDur);
        if (grid)
            Add(inT, "TranslateY", 10, 0, inDur);
        else
            Add(inT, "TranslateX", -12, 0, inDur);

        sb.Completed += (_, _) =>
        {
            if (gen != _viewAnimGen) return;
            outgoing.Visibility = Visibility.Collapsed;
        };
        _viewAnim = sb;
        sb.Begin();
    }

    private void OnViewList(object sender, RoutedEventArgs e)
    {
        SetView(false);
        SaveViewPreference();
    }

    private void OnViewGrid(object sender, RoutedEventArgs e)
    {
        SetView(true);
        SaveViewPreference();
    }

    /// <summary>记住用户视图偏好，下次启动沿用。</summary>
    private void SaveViewPreference()
    {
        LocalState.Ui.DefaultView = _gridView ? "grid" : "list";
        LocalState.SaveUi();
    }

    // ==================== 设置 ====================

    /// <summary>主界面 ↔ 设置页切换。动画：打开 = 主页左滑淡出 + 设置页从右滑入（交叉），
    /// 关闭 = 设置页右滑淡出 + 主页从左滑入；减少动画 / 窗口未上屏时瞬时切换（与收藏动画同一策略）。</summary>
    private void ApplyModeSwitch(bool open, bool animate)
    {
        if (!animate)
        {
            _modeOutMain.Stop(); _modeInMain.Stop(); _modeOutSet.Stop(); _modeInSet.Stop();
            MainPanel.Visibility = open ? Visibility.Collapsed : Visibility.Visible;
            SettingsPanel.Visibility = open ? Visibility.Visible : Visibility.Collapsed;
            ResetModeTransform(MainPanel, MainShift, MainScale);
            ResetModeTransform(SettingsPanel, SetShift, SetScale);
            _modeAnimating = false;
            return;
        }
        if (_modeAnimating) return;  // 220ms 内连点/连按 Esc 忽略，避免状态错乱

        _modeAnimating = true;
        if (open)
        {
            // 主页保持可见并左滑淡出；设置页先就位（透明 + 右侧 22px）再淡入，两段交叉
            MainPanel.Visibility = Visibility.Visible;
            SettingsPanel.Visibility = Visibility.Visible;
            SettingsPanel.Opacity = 0;
            SetShift.X = 22;
            SetScale.ScaleX = SetScale.ScaleY = 0.98;
            _modeOutMain.Begin();
            _modeInSet.Begin();
        }
        else
        {
            // 设置页右滑淡出；主页先就位（透明 + 左侧 22px）再淡入，两段交叉
            MainPanel.Visibility = Visibility.Visible;
            MainPanel.Opacity = 0;
            MainShift.X = -22;
            MainScale.ScaleX = MainScale.ScaleY = 0.98;
            _modeInMain.Begin();
            _modeOutSet.Begin();
        }
    }

    private void OnOpenSettings(object sender, RoutedEventArgs e)
    {
        SyncSettingsUi();
        ShowPane("general");
        _ = LoadBuiltinsAsync();   // 内置命令清单（host 不可达则降级为空列表，筛选行随之隐藏）
        // SettingsPanel 无背景（玻璃下透壁纸），主页必须隐藏，否则内容从透明区透出来叠在一起
        ApplyModeSwitch(open: true, animate: _visible);
        UpdateDragRegions();
    }

    /// <summary>设置页"命令"栏：从 host 拉取命令表（含图标路径），失败/未连接时降级为空列表并隐藏筛选行。</summary>
    private async Task LoadBuiltinsAsync()
    {
        try
        {
            var items = await _host.GetBuiltinsAsync();
            foreach (var it in items)
            {
                _ = LoadBuiltinListIconAsync(it);
            }
            _allBuiltins.Clear();
            _allBuiltins.AddRange(items);
        }
        catch (Exception ex)
        {
            // GetBuiltinsAsync 内部吞错返回空表，理论不可达；兜底清空，避免残留过期清单
            App.Log("LoadBuiltins", ex);
            _allBuiltins.Clear();
        }
        finally
        {
            // 显隐收敛在每次拉取后无条件执行（GetBuiltinsAsync 从不抛错，空表 = 未连接/出错），
            // 保证 host 不可达时筛选框与空态互斥不变量成立，也避免空表分支在主路径上死代码化
            ApplyBuiltinFilter();
        }
    }

    private readonly List<BuiltinInfoDto> _allBuiltins = new();

    private void OnBuiltinFilterChanged(object sender, object e)
    {
        // 全字段空引用短路：文本事件可能在窗口初始化早期触发，此时列表字段可能未就绪
        if (BuiltinFilterBox is null || BuiltinList is null || BuiltinFilterEmpty is null) return;
        ApplyBuiltinFilter();
    }

    /// <summary>
    /// 命令清单筛选：关键词（名称/别名/拼音缩写/说明/id）仅内存过滤 _allBuiltins（数据源不动），
    /// ItemsSource 指向筛选副本。与市场筛选同款交互；无清单时整行隐藏。
    /// </summary>
    private void ApplyBuiltinFilter()
    {
        if (_allBuiltins.Count == 0)
        {
            BuiltinFilterBox.Visibility = Visibility.Collapsed;
            BuiltinList.ItemsSource = null;
            BuiltinList.Visibility = Visibility.Collapsed;
            BuiltinFilterEmpty.Visibility = Visibility.Collapsed;
            return;
        }

        BuiltinFilterBox.Visibility = Visibility.Visible;
        var kw = BuiltinFilterBox.Text?.Trim() ?? "";
        var view = new List<BuiltinInfoDto>(_allBuiltins.Count);
        foreach (var it in _allBuiltins)
        {
            if (MatchesBuiltinFilter(it, kw)) view.Add(it);
        }

        // 逐键筛选：筛选结果与当前视图逐项一致时跳过 ItemsSource 重赋值（整表替换会
        // 重建容器并重置滚动位置，与市场筛选同一抖动源处理）。
        var skipReassign = false;
        if (BuiltinList.ItemsSource is List<BuiltinInfoDto> cur && cur.Count == view.Count)
        {
            skipReassign = true;
            for (var i = 0; i < view.Count; i++)
            {
                if (!ReferenceEquals(cur[i], view[i])) { skipReassign = false; break; }
            }
        }
        if (!skipReassign) BuiltinList.ItemsSource = view;

        BuiltinList.Visibility = view.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        BuiltinFilterEmpty.Visibility = view.Count > 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    private static bool MatchesBuiltinFilter(BuiltinInfoDto it, string keyword)
    {
        if (keyword.Length == 0) return true;
        return ContainsIgnoreCase(it.Title, keyword)
            || ContainsIgnoreCase(it.Subtitle, keyword)
            || ContainsIgnoreCase(it.Id, keyword)
            || it.Aliases?.Any(a => ContainsIgnoreCase(a, keyword)) == true;
    }

    /// <summary>命令栏行图标：有系统图标路径就提取显示，否则保持字形。</summary>
    private async Task LoadBuiltinListIconAsync(BuiltinInfoDto item)
    {
        try
        {
            var src = await AppIconService.GetIconAsync(item.Id, item.IconPath);
            if (src is not null)
            {
                item.IconImage = src;
            }
        }
        catch (Exception ex)
        {
            App.Log("BuiltinIcon", ex);
        }
    }

    private void OnCloseSettings(object sender, RoutedEventArgs e)
    {
        ApplyModeSwitch(open: false, animate: _visible);
        QueryBox.Focus(FocusState.Programmatic);
        UpdateDragRegions();
    }

    private void OnSettingsKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Escape)
        {
            e.Handled = true;
            OnCloseSettings(sender, e);
        }
    }

    private void OnSettingsNav(object sender, RoutedEventArgs e)
    {
        var pane = (string)((Button)sender).Tag;
        ShowPane(pane);
        // 插件清单是 host 侧动态状态，每次进入插件页都重新拉一次。
        if (pane == "plugins") _ = LoadPluginsAsync();
    }

    /// <summary>设置页 pane 切换过渡：旧 pane 淡出上移 → 新 pane 从下方淡入升起（轻量，对齐收藏分组切换思路）。
    /// 快速连点/动画中再点直接落定；减少动画时瞬时切换。</summary>
    private bool _paneAnimating;
    private int _paneAnimGen;
    private readonly Dictionary<StackPanel, TranslateTransform> _paneShifts = new();

    private void ShowPane(string pane)
    {
        // 导航项选中态：背景胶囊 + 文字/图标提亮（图标颜色不自动继承 Button.Foreground，需显式同步）
        var res = Root.Resources;
        foreach (var (b, icon) in new (Button, FontIcon?)[]
        {
            (NavGeneral, NavIconGeneral),
            (NavAppearance, NavIconAppearance), (NavBuiltins, NavIconBuiltins),
            (NavPlugins, NavIconPlugins),
            (NavAbout, NavIconAbout),
        })
        {
            var on = (string)b.Tag == pane;
            b.Background = on ? (Brush)res["AccentSoftBrush"] : new SolidColorBrush(Colors.Transparent);
            b.Foreground = on ? (Brush)res["TextPrimaryBrush"] : (Brush)res["TextSecondaryBrush"];
            // 不加粗：SemiBold ↔ Normal 切换会让文字宽度变化，选中时看起来抖动
            if (icon is not null)
                icon.Foreground = on ? (Brush)res["TextPrimaryBrush"] : (Brush)res["TextSecondaryBrush"];
        }

        var target = pane switch
        {
            "appearance" => PaneAppearance,
            "builtins" => PaneBuiltins,
            "plugins" => PanePlugins,
            "about" => PaneAbout,
            _ => PaneGeneral,
        };
        if (target.Visibility == Visibility.Visible) return;  // 重复点当前项

        if (!_visible || _paneAnimating)
        {
            ApplyPaneInstant(target);
            return;
        }
        var current = _paneShifts.Keys.FirstOrDefault(p => p.Visibility == Visibility.Visible);
        if (current is null || current == target)
        {
            ApplyPaneInstant(target);
            return;
        }
        StartPaneTransition(current, target);
    }

    /// <summary>瞬时落定到目标 pane（复位所有 pane 的视觉状态）。</summary>
    private void ApplyPaneInstant(StackPanel target)
    {
        _paneAnimGen++;
        _paneAnimating = false;
        foreach (var (p, shift) in _paneShifts)
        {
            p.Visibility = p == target ? Visibility.Visible : Visibility.Collapsed;
            p.Opacity = 1;
            shift.Y = 0;
        }
    }

    /// <summary>旧 pane 淡出（120ms 上移）→ 新 pane 淡入（160ms 从下方升起）。</summary>
    private void StartPaneTransition(StackPanel current, StackPanel target)
    {
        var gen = ++_paneAnimGen;
        _paneAnimating = true;

        current.Opacity = 1;
        _paneShifts[current].Y = 0;
        var outSb = new Storyboard();
        PaneFade(outSb, current, _paneShifts[current], 1, 0, 0, -4, 120, new CubicEase { EasingMode = EasingMode.EaseIn });
        outSb.Completed += (_, _) =>
        {
            if (gen != _paneAnimGen) return;  // 已落定/再次切换，跳过
            current.Visibility = Visibility.Collapsed;
            _paneShifts[current].Y = 0;

            target.Visibility = Visibility.Visible;
            target.Opacity = 0;
            _paneShifts[target].Y = 8;
            var inSb = new Storyboard();
            PaneFade(inSb, target, _paneShifts[target], 0, 1, 8, 0, 160, new CubicEase { EasingMode = EasingMode.EaseOut });
            inSb.Completed += (_, _) =>
            {
                if (gen != _paneAnimGen) return;
                target.Opacity = 1;
                _paneShifts[target].Y = 0;
                _paneAnimating = false;
            };
            inSb.Begin();
        };
        outSb.Begin();
    }

    /// <summary>为 Storyboard 添加 pane 透明度 + 位移两段动画。</summary>
    private static void PaneFade(Storyboard sb, DependencyObject panel, DependencyObject shift,
        double op0, double op1, double y0, double y1, int ms, EasingFunctionBase ease)
    {
        var d = new Duration(TimeSpan.FromMilliseconds(ms));
        void Add(DependencyObject t, string prop, double from, double to)
        {
            var a = new DoubleAnimation { From = from, To = to, Duration = d, EasingFunction = ease };
            Storyboard.SetTarget(a, t);
            Storyboard.SetTargetProperty(a, prop);
            sb.Children.Add(a);
        }
        Add(panel, "Opacity", op0, op1);
        Add(shift, "Y", y0, y1);
    }

    /// <summary>打开设置时把 LocalState 同步到控件（期间不触发保存副作用）。
    /// 开机启动以注册表实际状态为准（用户可能在任务管理器里手动改过），LocalState 仅作缓存。</summary>
    private void SyncSettingsUi()
    {
        _syncing = true;
        try
        {
            StartupSwitch.IsChecked = LocalState.Ui.LaunchOnStartup;
            BallSwitch.IsChecked = LocalState.Ui.FloatingBallEnabled;
            DevModeSwitch.IsChecked = LocalState.Ui.DeveloperMode;
            ThemeCombo.SelectedIndex = LocalState.Ui.Theme switch { "light" => 2, "dark" => 1, _ => 0 };
            ViewCombo.SelectedIndex = LocalState.Ui.DefaultView == "grid" ? 1 : 0;
            UpdateHotkeyPresets(animate: false);
        }
        finally { _syncing = false; }
    }

    // ==================== 开机启动 ====================

    /// <summary>HKCU Run 键：当前用户登录时自动启动（无需管理员权限，卸载/禁用也不影响其他用户）。</summary>
    private const string RunKeyPath = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string RunValueName = "Spark";

    /// <summary>注册表 Run 键里是否存在 Spark 启动项（任务管理器「启动」页可见）。</summary>
    private static bool GetStartupEntry()
    {
        try
        {
            using var k = Registry.CurrentUser.OpenSubKey(RunKeyPath);
            return k?.GetValue(RunValueName) is string s && !string.IsNullOrEmpty(s);
        }
        catch { return false; }
    }

    /// <summary>
    /// 定位 spark-host.exe：优先 UI 同目录（安装布局 {app}\Spark.exe + {app}\spark-host.exe）；
    /// 开发布局回退到仓库根 target/{debug,release}\spark-host.exe（详见 HostIpcClient.FindHostExe）。
    /// 返回 null 表示找不到 host。
    /// </summary>
    private static string? FindHostExe() => HostIpcClient.FindHostExe();

    /// <summary>写入/删除开机启动项：true = 注册 host 路径（带引号，路径含空格时注册表才认得）。
    /// 必须写 spark-host.exe 而不是 Spark.exe：热键/托盘/索引都在 host 里，
    /// 开机只拉起 UI 的话热键不生效（host 会自行以 --hidden 拉起 UI）。
    /// 开发布局找不到 host 时退回 UI exe，保证开关在 dev 下仍可用。</summary>
    private static void SetStartupEntry(bool enable)
    {
        try
        {
            using var k = Registry.CurrentUser.CreateSubKey(RunKeyPath);
            if (k is null) return;
            if (enable)
            {
                var exe = FindHostExe()
                    ?? Environment.ProcessPath
                    ?? Path.Combine(AppContext.BaseDirectory, "Spark.exe");
                k.SetValue(RunValueName, $"\"{exe}\"");
            }
            else
            {
                k.DeleteValue(RunValueName, throwOnMissingValue: false);
            }
        }
        catch (Exception ex) { App.Log("StartupRegistry", ex); }
    }

    /// <summary>旧版本把 Run 项写成 UI（Spark.exe），开机只拉起 UI、host 缺失导致热键失效。
    /// 每次启动时检测并自动迁移到同目录的 spark-host.exe；只迁移指向 Spark.exe 的旧项，
    /// 用户手动改过的值不动。</summary>
    private static void MigrateStartupEntry()
    {
        try
        {
            using var k = Registry.CurrentUser.OpenSubKey(RunKeyPath);
            if (k?.GetValue(RunValueName) is not string s || string.IsNullOrEmpty(s)) return;
            var target = s.Trim().Trim('"');
            if (target.Contains("spark-host.exe", StringComparison.OrdinalIgnoreCase)) return;
            if (!Path.GetFileName(target).Equals("Spark.exe", StringComparison.OrdinalIgnoreCase)) return;
            var dir = Path.GetDirectoryName(target);
            if (string.IsNullOrEmpty(dir)) return;
            var host = Path.Combine(dir, "spark-host.exe");
            if (!File.Exists(host)) return;
            using var w = Registry.CurrentUser.CreateSubKey(RunKeyPath);
            w?.SetValue(RunValueName, $"\"{host}\"");
        }
        catch (Exception ex) { App.Log("StartupRegistry", ex); }
    }

    // ==================== 开关视觉（原型 .switch：OFF rgba(120,120,128,.32) / ON #30d158，transition 0.2s） ====================

    private static readonly Color SwitchOnColor = Color.FromArgb(0xFF, 0x30, 0xD1, 0x58);
    private static readonly Color SwitchOffColor = Color.FromArgb(0x52, 0x78, 0x78, 0x80);

    /// <summary>开关切换过渡：轨道颜色渐变 + 滑块滑动（200ms ease-out，对齐原型 transition 0.2s）。
    /// 视觉由代码驱动（不用模板内 VSM）：初始化（_syncing）时直接落定终值，用户点击时播动画。
    /// 模板元素经视觉树根 FindName 获取（模板命名作用域内可解析）。</summary>
    private void AnimateSwitchToggle(ToggleButton toggle, bool on, bool animate)
    {
        if (VisualTreeHelper.GetChildrenCount(toggle) == 0) return;
        var root = (FrameworkElement)VisualTreeHelper.GetChild(toggle, 0);
        var thumbT = root.FindName("ThumbTransform") as TranslateTransform;
        var track = root.FindName("Track") as Border;
        if (thumbT is null || track?.Background is not SolidColorBrush bg) return;

        var toX = on ? 16.0 : 0.0;
        var toColor = on ? SwitchOnColor : SwitchOffColor;
        if (!animate)
        {
            thumbT.X = toX;
            bg.Color = toColor;
            return;
        }

        var sb = new Storyboard();
        var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
        var d = new Duration(TimeSpan.FromMilliseconds(200));
        var x = new DoubleAnimation { From = thumbT.X, To = toX, Duration = d, EasingFunction = ease };
        Storyboard.SetTarget(x, thumbT);
        Storyboard.SetTargetProperty(x, "X");
        sb.Children.Add(x);
        var c = new ColorAnimation { From = bg.Color, To = toColor, Duration = d, EasingFunction = ease };
        Storyboard.SetTarget(c, bg);
        Storyboard.SetTargetProperty(c, "Color");
        sb.Children.Add(c);
        sb.Begin();
    }

    private void OnToggleStartup(object sender, RoutedEventArgs e)
    {
        var on = StartupSwitch.IsChecked == true;
        AnimateSwitchToggle(StartupSwitch, on, animate: !_syncing);
        if (_syncing) return;
        try
        {
            LocalState.Ui.LaunchOnStartup = on;
            SetStartupEntry(on);
            LocalState.SaveUi();
            QueueHostConfigUpdate(x => x.LaunchOnStartup = on);
        }
        catch (Exception ex) { App.Log("StartupConfig", ex); }
    }

    // ==================== 悬浮球 ====================

    private void OnToggleBall(object sender, RoutedEventArgs e)
    {
        var on = BallSwitch.IsChecked == true;
        AnimateSwitchToggle(BallSwitch, on, animate: !_syncing);
        if (_syncing) return;
        LocalState.Ui.FloatingBallEnabled = on;
        LocalState.SaveUi();
        if (on) ShowBall();
        else HideBall();
    }

    // ==================== 开发者模式 ====================

    /// <summary>开发者模式关闭时禁用的插件管理入口（灰显 + 提示，而非隐藏——让用户能发现功能存在）。
    /// "加载开发目录"会绕过正式安装流程直接跑任意本地目录代码；"更换插件目录"会做实际的目录迁移，
    /// 两者风险高于安装本地插件包/启停/卸载/权限授权，因此单独收编到开发者模式下。</summary>
    private void ApplyDevModeGate(bool devMode)
    {
        DevLoadPluginBtn.IsEnabled = devMode;
        ChangePluginDirBtn.IsEnabled = devMode;
        var tip = devMode ? null : "需先开启「设置 → 通用 → 开发者模式」";
        // 禁用按钮本身不接收 hover（IsHitTestVisible 随 IsEnabled=false 失效），tooltip 不弹；
        // 把提示挂到可 hit-test 的外层包装上，灰显按钮 hover 时仍能显示引导。
        ToolTipService.SetToolTip(DevLoadPluginWrap, tip);
        ToolTipService.SetToolTip(ChangePluginDirWrap, tip);
    }

    /// <summary>通用设置-开发者模式开关：开启后插件页每行显示"调试"按钮，
    /// 且解锁"加载开发目录""更换插件目录"两个高风险入口；关闭时二者灰显但不隐藏。
    /// 立即重建插件列表刷新每行 DebugVisibility，不依赖切回插件页导航时重载。</summary>
    private void OnToggleDevMode(object sender, RoutedEventArgs e)
    {
        var on = DevModeSwitch.IsChecked == true;
        AnimateSwitchToggle(DevModeSwitch, on, animate: !_syncing);
        ApplyDevModeGate(on);
        if (_syncing) return;
        LocalState.Ui.DeveloperMode = on;
        LocalState.SaveUi();
        App.Log("DevMode", $"toggled on={on}");
        if (_pluginRows.Count > 0) _ = LoadPluginsAsync();
    }

    /// <summary>创建并驻留悬浮球（独立置顶小窗）。失败时回弹开关与设置，避免
    /// 「开关开着但球不在」的假状态（用户可随时再开重试），不弹窗打断。</summary>
    private void ShowBall()
    {
        if (_ball is not null) return;
        try
        {
            var dark = LocalState.Ui.Theme switch { "light" => false, "dark" => true, _ => SystemUsesDark() };
            _ball = new FloatingBallWindow(
                dark,
                onToggle: () => DispatcherQueue.TryEnqueue(() =>
                {
                    // 点击悬浮球 = 唤起/隐藏主窗口（与热键一致；byToggle 绕过显示保护期）
                    if (_visible) HideLauncher(byToggle: true);
                    else ShowLauncher();
                }),
                onShow: () => DispatcherQueue.TryEnqueue(ShowLauncher),
                onExit: () => DispatcherQueue.TryEnqueue(OnHostExit));
            App.Log("Ball", "floating ball created");
        }
        catch (Exception ex)
        {
            App.Log("Ball", ex);
            _ball = null;
            // 回弹开关与设置（_syncing 下 IsChecked 赋值不触发 OnToggleBall 副作用；
            // finally 保证即使赋值抛异常也不会卡死设置页所有开关）
            try
            {
                _syncing = true;
                LocalState.Ui.FloatingBallEnabled = false;
                LocalState.SaveUi();
                BallSwitch.IsChecked = false;
            }
            finally { _syncing = false; }
        }
    }

    private void HideBall()
    {
        try { _ball?.Dispose(); } catch (Exception ex) { App.Log("Ball", ex); }
        _ball = null;
    }

    private void OnThemeChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing) return;
        LocalState.Ui.Theme = ThemeCombo.SelectedIndex switch { 1 => "dark", 2 => "light", _ => "system" };
        LocalState.SaveUi();
        ApplyTheme();
    }

    private void OnViewChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing) return;
        LocalState.Ui.DefaultView = ViewCombo.SelectedIndex == 1 ? "grid" : "list";
        LocalState.SaveUi();
        SetView(LocalState.Ui.DefaultView == "grid");
    }

    private void OnHotkeyPreset(object sender, RoutedEventArgs e)
    {
        LocalState.Ui.Hotkey = (string)((Button)sender).Tag;
        LocalState.SaveUi();
        UpdateHotkeyPresets(animate: true);
        // 热键由 host 注册：推送到 host 重注册（host 不可达时重连后自动补同步）
        QueueHostConfigUpdate(x => x.HotkeyToggle = LocalState.Ui.Hotkey);
    }

    /// <summary>预设按钮选中态：背景/边框颜色 200ms 渐变切换（对齐开关动画）。
    /// 文字颜色两态一致——纯白在深底上观感像加粗，选中只靠底色 + 描边表达。</summary>
    private void UpdateHotkeyPresets(bool animate)
    {
        var res = Root.Resources;
        foreach (var b in new[] { BtnHotkeyAlt, BtnHotkeyCtrl })
        {
            var on = (string)b.Tag == LocalState.Ui.Hotkey;
            AnimateBrush(b, Control.BackgroundProperty, b.Background,
                on ? (Brush)res["AccentSoftBrush"] : (Brush)res["ChipBgBrush"], animate);
            AnimateBrush(b, Control.BorderBrushProperty, b.BorderBrush,
                on ? (Brush)res["AccentBrush"] : (Brush)res["GlassBorderBrush"], animate);
        }
    }

    /// <summary>把按钮画刷替换为当前色的独立副本（共享资源画刷不能直接动画），再渐变到目标色；animate=false 直接落定。</summary>
    private static void AnimateBrush(Button b, DependencyProperty dp, Brush current, Brush target, bool animate)
    {
        if (!animate || current is not SolidColorBrush from || target is not SolidColorBrush to)
        {
            b.SetValue(dp, target);
            return;
        }
        var clone = new SolidColorBrush(from.Color);
        b.SetValue(dp, clone);
        var a = new ColorAnimation
        {
            From = from.Color,
            To = to.Color,
            Duration = new Duration(TimeSpan.FromMilliseconds(200)),
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
        };
        Storyboard.SetTarget(a, clone);
        Storyboard.SetTargetProperty(a, "Color");
        var sb = new Storyboard();
        sb.Children.Add(a);
        sb.Begin();
    }

    // ==================== 插件 ====================

    private readonly List<PluginRowVm> _pluginRows = new();

    /// <summary>拉插件清单 + 当前插件目录，重绘列表/空态。</summary>
    private async Task LoadPluginsAsync()
    {
        try
        {
            var list = await _host.PluginListAsync();
            _pluginRows.Clear();
            _pluginRows.AddRange(list.Select(p => new PluginRowVm(p)));

            PluginList.ItemsSource = null;
            PluginList.ItemsSource = _pluginRows;

            var empty = _pluginRows.Count == 0;
            PluginEmpty.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
            PluginList.Visibility = empty ? Visibility.Collapsed : Visibility.Visible;

            var cfg = await _host.GetConfigAsync();
            PluginDirText.Text = string.IsNullOrWhiteSpace(cfg?.PluginsDir)
                ? "默认（安装目录 / plugins）"
                : cfg!.PluginsDir!;
            if (cfg is not null) RefreshTrustedDevUi(cfg);
        }
        catch (Exception ex)
        {
            App.Log("LoadPlugins", ex);
            SetPluginStatus("插件清单加载失败：" + ex.Message);
        }
    }

    /// <summary>ListView 容器上屏：SwitchStyle 的 ON 态颜色靠代码驱动，
    /// 容器回收/滚动复用时需把开关刷到正确初始态（无动画）。</summary>
    private void OnPluginItemRealized(ListViewBase sender, ContainerContentChangingEventArgs args)
    {
        if (args.ItemContainer is not ListViewItem container) return;
        if (args.Item is not PluginRowVm row) return;
        if (FindToggleInContainer(container) is not { } toggle) return;
        // 容器复用时 IsChecked 绑定已是正确值，但轨道颜色/滑块位置需手动同步
        AnimateSwitchToggle(toggle, row.Enabled, animate: false);
    }

    /// <summary>在 ListViewItem 视觉树里找 ToggleButton（FindDescendant 在 1.6 不可用，手写遍历）。</summary>
    private static ToggleButton? FindToggleInContainer(FrameworkElement root)
    {
        var count = VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < count; i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is ToggleButton tb) return tb;
            if (child is FrameworkElement fe && FindToggleInContainer(fe) is { } nested)
                return nested;
        }
        return null;
    }

    private void SetPluginStatus(string? text)
    {
        PluginStatus.Text = text ?? "";
        PluginStatus.Visibility = string.IsNullOrEmpty(text)
            ? Visibility.Collapsed
            : Visibility.Visible;
    }

    private async void OnRefreshPlugins(object sender, RoutedEventArgs e)
    {
        SetPluginStatus(null);
        await LoadPluginsAsync();
    }

    private async void OnInstallPlugin(object sender, RoutedEventArgs e)
    {
        var dir = await PickFolderAsync("选择插件目录（含 plugin.json）");
        if (dir is null) return;
        try
        {
            var outcome = await _host.PluginInstallAsync(dir);
            switch (outcome.Action)
            {
                case "installed":
                    SetPluginStatus($"已安装{SignSuffix(outcome.SignState)}：{outcome.Id}");
                    break;
                case "updated":
                    PluginWindowHost.CloseIfOpen(outcome.Id);
                    SetPluginStatus($"已更新到 v{outcome.Version}{SignSuffix(outcome.SignState)}");
                    break;
                case "confirm_downgrade":
                {
                    var msg = $"检测到旧版本\n已装 v{outcome.PreviousVersion}，将安装 v{outcome.Version}\n是否继续覆盖安装？";
                    if (!await ConfirmDestructiveAsync(msg))
                    {
                        SetPluginStatus("已取消");
                        break;
                    }
                    PluginWindowHost.CloseIfOpen(outcome.Id);
                    var forced = await _host.PluginInstallAsync(dir, force: true);
                    SetPluginStatus(forced.Action == "updated"
                        ? $"已降级安装到 v{forced.Version}{SignSuffix(forced.SignState)}"
                        : $"已安装{SignSuffix(forced.SignState)}：{forced.Id}");
                    break;
                }
                default:
                    SetPluginStatus($"已安装{SignSuffix(outcome.SignState)}：{outcome.Id}");
                    break;
            }
            await LoadPluginsAsync();
        }
        catch (Exception ex)
        {
            App.Log("PluginInstall", ex);
            SetPluginStatus("安装失败：" + ex.Message);
        }
    }

    /// <summary>安装成功提示后的签名状态后缀。Invalid 在 install 阶段已被 host 拒装（抛错），不会走到这里；仍兜底返回空。</summary>
    private static string SignSuffix(string? signState) => signState?.ToLowerInvariant() switch
    {
        "official" => "（官方）",
        "third_party" => "（已签名）",
        _ => "",
    };

    // ─── 签名安全：严格模式 + 受信任开发者（3.2/3.3）────────────────────────

    private readonly ObservableCollection<TrustedDevVm> _trustedDevs = new();
    /// <summary>回写守卫：RefreshTrustedDevUi 填充控件 + OnToggleStrictMode 失败回滚时，
    /// 避免 IsChecked/ItemsSource 赋值触发事件回推 host（与 OnToggleBall 的 _syncing 同范式）。</summary>
    private bool _loadingPluginConfig;

    /// <summary>设置页展示用：一条受信任第三方公钥。</summary>
    private sealed class TrustedDevVm
    {
        public TrustedDevVm(TrustedPubkeyDto dto)
        {
            KeyId = dto.KeyId;
            Note = string.IsNullOrWhiteSpace(dto.Note) ? "（无备注）" : dto.Note;
            PublicKeyPreview = dto.PublicKey.Length > 24
                ? dto.PublicKey[..12] + "…" + dto.PublicKey[^12..]
                : dto.PublicKey;
            _dto = dto;
        }

        public string KeyId { get; }
        public string Note { get; }
        public string PublicKeyPreview { get; }
        public TrustedPubkeyDto Dto => _dto;
        private readonly TrustedPubkeyDto _dto;
    }

    /// <summary>从 host config 刷新严格模式开关与受信任开发者列表（host 为权威，UI 只展示）。</summary>
    private void RefreshTrustedDevUi(HostConfigDto cfg)
    {
        _loadingPluginConfig = true;
        try
        {
            StrictModeSwitch.IsChecked = cfg.StrictMode;
            _trustedDevs.Clear();
            foreach (var dto in cfg.TrustedPubkeys)
                _trustedDevs.Add(new TrustedDevVm(dto));
            TrustedDevList.ItemsSource = null;
            TrustedDevList.ItemsSource = _trustedDevs;
        }
        finally { _loadingPluginConfig = false; }
    }

    /// <summary>进入"插件安全"tab 时拉取 host 配置刷新严格模式开关与受信任开发者列表。</summary>
    private async Task RefreshPluginSecurityAsync()
    {
        try
        {
            var cfg = await _host.GetConfigAsync();
            if (cfg is not null) RefreshTrustedDevUi(cfg);
        }
        catch (Exception ex)
        {
            App.Log("RefreshPluginSecurity", ex);
            SetPluginStatus("加载安全设置失败：" + ex.Message);
        }
    }

    private async void OnToggleStrictMode(object sender, RoutedEventArgs e)
    {
        if (_loadingPluginConfig) return;
        var on = StrictModeSwitch.IsChecked == true;
        if (!await _host.SetConfigAsync(new HostConfigUpdate { StrictMode = on }))
        {
            // 程序化回滚 IsChecked 会重入 Checked/Unchecked 事件（与 OnToggleBall 的
            // _syncing 守卫同范式），用 _loadingPluginConfig 挡住重入避免无限乒乓。
            _loadingPluginConfig = true;
            try { StrictModeSwitch.IsChecked = !on; }
            finally { _loadingPluginConfig = false; }
            SetPluginStatus("严格模式设置失败（host 未接受），请重试");
            return;
        }
        SetPluginStatus(on ? "严格模式已开启：仅安装带有效签名的插件" : "严格模式已关闭");
    }

    private async void OnAddTrustedDev(object sender, RoutedEventArgs e)
    {
        var keyId = TrustedDevKeyId.Text.Trim();
        var pubkey = TrustedDevPubkey.Text.Trim();
        var note = "";
        string? err = null;
        if (keyId.Length == 0) err = "请填 key_id（开发者公钥标识）";
        else if (keyId.Length > 128) err = "key_id 过长（>128 字符）";
        else if (keyId.Any(char.IsWhiteSpace)) err = "key_id 不能含空白字符";
        else if (keyId == Models.RegistrySignatureDto.OfficialKeyId) err = "该 key_id 与内置官方密钥冲突，不可导入";
        else if (_trustedDevs.Any(t => t.KeyId == keyId)) err = $"key_id「{keyId}」已存在";
        if (err is null)
        {
            if (pubkey.Length == 0) err = "请粘贴 base64 公钥";
            else
            {
                var compact = pubkey.Replace(" ", "").Replace("\t", "").Replace("\r", "").Replace("\n", "");
                try
                {
                    var bytes = Convert.FromBase64String(compact);
                    if (bytes.Length != 32) err = $"公钥必须解码为 32 字节（Ed25519），实际 {bytes.Length} 字节";
                    else pubkey = compact; // 规整后回写，避免 host 侧 trim 语义差异
                }
                catch (FormatException) { err = "公钥不是合法 base64"; }
            }
        }
        if (err is not null)
        {
            TrustedDevStatus.Text = err;
            TrustedDevStatus.Visibility = Visibility.Visible;
            return;
        }

        var dto = new TrustedPubkeyDto { KeyId = keyId, PublicKey = pubkey, Note = note };
        var next = _trustedDevs.Select(t => t.Dto).Append(dto).ToList();
        if (!await PushTrustedDevsAsync(next))
        {
            TrustedDevStatus.Text = "添加失败（host 拒绝该公钥，请检查格式）";
            TrustedDevStatus.Visibility = Visibility.Visible;
            return;
        }
        _trustedDevs.Add(new TrustedDevVm(dto));
        TrustedDevKeyId.Text = "";
        TrustedDevPubkey.Text = "";
        TrustedDevStatus.Text = $"已信任开发者「{keyId}」：其签名的插件将显示\"已签名\"";
        TrustedDevStatus.Visibility = Visibility.Visible;
    }

    private async void OnRemoveTrustedDev(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { DataContext: TrustedDevVm vm }) return;
        var next = _trustedDevs.Where(t => t != vm).Select(t => t.Dto).ToList();
        if (!await PushTrustedDevsAsync(next))
        {
            TrustedDevStatus.Text = $"移除「{vm.KeyId}」失败，请重试";
            TrustedDevStatus.Visibility = Visibility.Visible;
            return;
        }
        _trustedDevs.Remove(vm);
        TrustedDevStatus.Text = $"已移除受信任开发者「{vm.KeyId}」";
        TrustedDevStatus.Visibility = Visibility.Visible;
    }

    /// <summary>整表推送受信任开发者到 host（host.set_config 全量替换并整体校验）。</summary>
    private async Task<bool> PushTrustedDevsAsync(List<TrustedPubkeyDto> list)
    {
        try
        {
            return await _host.SetConfigAsync(new HostConfigUpdate { TrustedPubkeys = list });
        }
        catch (Exception ex)
        {
            App.Log("SetTrustedDevs", ex);
            SetPluginStatus("受信任开发者设置失败：" + ex.Message);
            return false;
        }
    }

    private async void OnDevLoadPlugin(object sender, RoutedEventArgs e)
    {
        var dir = await PickFolderAsync("选择开发目录（不拷贝，改文件即生效）");
        if (dir is null) return;
        try
        {
            var id = await _host.PluginDevLoadAsync(dir);
            SetPluginStatus($"已加载开发插件：{id}");
            await LoadPluginsAsync();
        }
        catch (Exception ex)
        {
            App.Log("PluginDevLoad", ex);
            SetPluginStatus("加载失败：" + ex.Message);
        }
    }

    /// <summary>更换插件目录；已装插件一并迁移过去（host 侧做实际搬运）。</summary>
    private async void OnChangePluginDir(object sender, RoutedEventArgs e)
    {
        var dir = await PickFolderAsync("选择新的插件目录");
        if (dir is null) return;
        if (!await ConfirmDestructiveAsync($"把插件目录改为\n{dir}\n并迁移现有插件？"))
            return;
        try
        {
            if (await _host.PluginSetDirAsync(dir, migrate: true))
            {
                SetPluginStatus("插件目录已更新");
                await LoadPluginsAsync();
            }
            else
            {
                SetPluginStatus("插件目录更新失败");
            }
        }
        catch (Exception ex)
        {
            App.Log("PluginSetDir", ex);
            SetPluginStatus("插件目录更新失败：" + ex.Message);
        }
    }

    /// <summary>卡片点击：切换展开/收起，展开时对详情区播 Opacity+位移动画、箭头旋转 180°。</summary>
    private void OnTogglePluginExpand(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PluginRowVm row) return;
        row.IsExpanded = !row.IsExpanded;

        if (sender is not FrameworkElement fe) return;

        // 箭头旋转过渡：展开 180°、收起 0°
        var rotate = FindNamed<RotateTransform>(fe, "ExpandArrowRotate");
        if (rotate is not null) AnimateArrowRotate(rotate, row.IsExpanded ? 180 : 0);

        // 展开时播详情区淡入动画（收起时 Visibility 瞬时消失，无动画）
        if (!row.IsExpanded) return;
        // 从 sender 逐级向上，在每个祖先子树里找命名详情区 PluginDetailPanel，
        // 找到即播动画。逐级向上避免误停在中间容器（标题行右侧 StackPanel 等也继承同一 DataContext）。
        var cur = fe;
        while (cur is not null)
        {
            var detail = FindNamed<StackPanel>(cur, "PluginDetailPanel");
            if (detail is not null && detail.Visibility == Visibility.Visible)
            {
                AnimateDetailExpand(detail);
                return;
            }
            cur = VisualTreeHelper.GetParent(cur) as FrameworkElement;
        }
    }

    /// <summary>箭头旋转：180ms EaseOut。</summary>
    private static void AnimateArrowRotate(RotateTransform rotate, double toAngle)
    {
        var sb = new Storyboard();
        var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
        var a = new DoubleAnimation { From = rotate.Angle, To = toAngle, Duration = new Duration(TimeSpan.FromMilliseconds(180)), EasingFunction = ease };
        Storyboard.SetTarget(a, rotate);
        Storyboard.SetTargetProperty(a, "Angle");
        sb.Children.Add(a);
        sb.Begin();
    }

    /// <summary>在元素子树里按 x:Name 查找命名元素（模板内 x:Name 在模板作用域，需遍历视觉树）。</summary>
    private static T? FindNamed<T>(FrameworkElement root, string name) where T : DependencyObject
    {
        var count = VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < count; i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T t && child is FrameworkElement fe && fe.Name == name) return t;
            if (child is FrameworkElement feChild && FindNamed<T>(feChild, name) is { } nested) return nested;
        }
        return null;
    }

    /// <summary>详情区淡入：Opacity 0→1 + Y 8→0，180ms。</summary>
    private static void AnimateDetailExpand(FrameworkElement detail)
    {
        detail.Opacity = 0;
        var shift = detail.RenderTransform as TranslateTransform;
        if (shift is null)
        {
            shift = new TranslateTransform();
            detail.RenderTransform = shift;
        }
        shift.Y = 8;

        var sb = new Storyboard();
        var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
        var d = new Duration(TimeSpan.FromMilliseconds(180));
        var op = new DoubleAnimation { From = 0, To = 1, Duration = d, EasingFunction = ease };
        Storyboard.SetTarget(op, detail);
        Storyboard.SetTargetProperty(op, "Opacity");
        sb.Children.Add(op);
        var y = new DoubleAnimation { From = 8, To = 0, Duration = d, EasingFunction = ease };
        Storyboard.SetTarget(y, shift);
        Storyboard.SetTargetProperty(y, "Y");
        sb.Children.Add(y);
        sb.Begin();
    }

    private async void OnPluginToggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleButton toggle) return;
        if (toggle.DataContext is not PluginRowVm row) return;
        var on = toggle.IsChecked == true;
        // 驱动 SwitchStyle 的滑块位移 + 轨道颜色（与通用页开关同一动画路径）
        AnimateSwitchToggle(toggle, on, animate: true);

        // 容器虚拟化：ToggleButton 上屏时绑定会补触发一次 Checked/Unchecked，值与 host 一致就是回声。
        if (row.Enabled == row.SyncedEnabled) return;

        var target = row.Enabled;

        // 签名失效的插件只允许"关闭"，不允许"启用"：阻止 Off→On 的拨动并回滚，
        // 但保留 On→Off（用户停用失效插件是最直接的处置，不能被锁死）。
        if (target && row.SignState == PluginSignState.Invalid)
        {
            row.Enabled = row.SyncedEnabled;
            AnimateSwitchToggle(toggle, row.SyncedEnabled, animate: true);
            SetPluginStatus($"{row.Name} 签名校验失败，无法启用");
            return;
        }

        if (await _host.PluginToggleAsync(row.Id, target))
        {
            row.SyncedEnabled = target;
            // 禁用后旧窗口若还开着，页面仍能调 spark.*（host 只按 granted 鉴权，不看 enabled），必须关掉。
            if (!target) PluginWindowHost.CloseIfOpen(row.Id);
        }
        else
        {
            row.Enabled = row.SyncedEnabled;   // 回滚开关，避免 UI 与 host 不一致
            AnimateSwitchToggle(toggle, row.SyncedEnabled, animate: true);
            SetPluginStatus($"{row.Name} 状态更新失败");
        }
    }

    private async void OnPermissionToggled(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PermissionVm perm) return;
        if (perm.Granted == perm.SyncedGranted) return;   // 绑定回声

        var row = _pluginRows.FirstOrDefault(r => r.Permissions.Contains(perm));
        if (row is null) return;

        // grant 是全量覆盖：把该插件当前所有已勾选的权限一起提交。
        var granted = row.Permissions.Where(p => p.Granted).Select(p => p.Key).ToList();
        if (await _host.PluginGrantAsync(row.Id, granted))
        {
            foreach (var p in row.Permissions) p.SyncedGranted = p.Granted;
            // 收回权限后已开着的窗口仍持有旧 granted 快照，关掉它强制重新取。
            if (!perm.Granted) PluginWindowHost.CloseIfOpen(row.Id);
            SetPluginStatus($"{row.Name} 权限已更新");
        }
        else
        {
            perm.Granted = perm.SyncedGranted;
            SetPluginStatus($"{row.Name} 权限更新失败");
        }
    }

    private async void OnUninstallPlugin(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PluginRowVm row) return;
        if (!await ConfirmDestructiveAsync($"卸载插件「{row.Name}」？\n插件目录将被删除。")) return;

        if (await _host.PluginUninstallAsync(row.Id))
        {
            // 目录已被删除，窗口里的页面资源随之失效，先收窗口再刷新列表。
            PluginWindowHost.CloseIfOpen(row.Id);
            SetPluginStatus($"已卸载：{row.Name}");
            await LoadPluginsAsync();
        }
        else
        {
            SetPluginStatus($"卸载失败：{row.Name}");
        }
    }

    /// <summary>插件卡片"调试"按钮：以 devMode=true 打开插件窗口（DevTools/右键菜单/加速键全开），
    /// 与正式安装/dev 目录无关——只要开发者模式开着就能调试任意已启用插件。
    /// 若该插件窗口已打开，先收掉再重开：OpenOrFocus 命中旧窗口时不会重应用 dev 设置，
    /// 直接聚焦会拿到 DevTools 未启用的旧实例，必须重开才能保证调试生效。</summary>
    private async void OnDebugPlugin(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PluginRowVm row) return;
        try
        {
            var info = await _host.PluginOpenAsync(row.Id, "", "");
            if (info is null)
            {
                SetPluginStatus($"无法打开调试：{row.Name}（插件未启用或非 WebView 类型）");
                return;
            }
            PluginWindowHost.CloseIfOpen(row.Id);
            PluginWindowHost.OpenOrFocus(info, _host, "", "", "", devMode: true);
            SetPluginStatus($"已打开调试窗口：{row.Name}");
        }
        catch (Exception ex)
        {
            App.Log("PluginDebug", ex);
            SetPluginStatus($"调试打开失败：{row.Name}");
        }
    }

    /// <summary>系统文件夹选择器；取消返回 null。WinUI3 需显式绑定 HWND。</summary>
    private async Task<string?> PickFolderAsync(string commitText)
    {
        try
        {
            var picker = new Windows.Storage.Pickers.FolderPicker
            {
                SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.ComputerFolder,
                CommitButtonText = commitText,
            };
            picker.FileTypeFilter.Add("*");
            InitializeWithWindow.Initialize(picker, _hwnd);
            var folder = await picker.PickSingleFolderAsync();
            return folder?.Path;
        }
        catch (Exception ex)
        {
            App.Log("PickFolder", ex);
            SetPluginStatus("打开文件夹选择器失败：" + ex.Message);
            return null;
        }
    }

    // ==================== 插件市场 ====================

    private readonly List<RegistryPluginViewDto> _marketPlugins = new();
    private readonly ObservableCollection<CustomRepoUrlVm> _customRepoUrls = new();
    private bool _marketLoaded;
    private bool _loadingMarket;
    /// <summary>当前仓库索引（zipball_url 等元数据来源），供安装时复用。</summary>
    private RegistryIndexDto? _currentRegistry;

    private sealed class CustomRepoUrlVm : INotifyPropertyChanged
    {
        private string _url = "";
        public string Url
        {
            get => _url;
            set { if (_url != value) { _url = value; PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Url))); } }
        }
        public event PropertyChangedEventHandler? PropertyChanged;
    }

    private void OnSelectInstalledPluginsTab(object sender, RoutedEventArgs e)
    {
        ActivatePluginsSubPane(SubPaneInstalledPlugins, TabInstalledPluginsBtn);
    }

    private async void OnSelectPluginSecurityTab(object sender, RoutedEventArgs e)
    {
        ActivatePluginsSubPane(SubPanePluginSecurity, TabPluginSecurityBtn);
        // 安全页数据不依赖已安装 tab；进入即拉一次 host config 刷新开关与公钥列表
        await RefreshPluginSecurityAsync();
    }

    private async void OnSelectMarketplaceTab(object sender, RoutedEventArgs e)
    {
        ActivatePluginsSubPane(SubPaneMarketplace, TabMarketplaceBtn);

        if (!_marketLoaded)
        {
            await InitMarketplaceAsync();
        }
    }

    private bool _subPaneAnimating;
    private int _subPaneAnimGen;

    /// <summary>三个插件子 tab 共用的面板切换：旧面板淡出上移 → 新面板淡入升起，加过渡动画。</summary>
    private void ActivatePluginsSubPane(StackPanel paneToShow, Button tabToActivate)
    {
        // 按钮态先切换（无动画开销）
        SetPluginsTabButtonState(TabInstalledPluginsBtn, tabToActivate == TabInstalledPluginsBtn);
        SetPluginsTabButtonState(TabPluginSecurityBtn, tabToActivate == TabPluginSecurityBtn);
        SetPluginsTabButtonState(TabMarketplaceBtn, tabToActivate == TabMarketplaceBtn);

        var current = CurrentSubPane();
        if (current == paneToShow) return;  // 重复点当前项，不动画

        if (current is null || _subPaneAnimating)
        {
            // 无当前面板或动画中 → 瞬时落定
            ApplySubPaneInstant(paneToShow);
            return;
        }

        StartSubPaneTransition(current, paneToShow);
    }

    private StackPanel? CurrentSubPane()
    {
        if (SubPaneInstalledPlugins.Visibility == Visibility.Visible) return SubPaneInstalledPlugins;
        if (SubPanePluginSecurity.Visibility == Visibility.Visible) return SubPanePluginSecurity;
        if (SubPaneMarketplace.Visibility == Visibility.Visible) return SubPaneMarketplace;
        return null;
    }

    private TranslateTransform SubPaneShiftOf(StackPanel p) => p switch
    {
        _ when p == SubPaneInstalledPlugins => SubPaneInstalledPluginsShift,
        _ when p == SubPanePluginSecurity => SubPanePluginSecurityShift,
        _ when p == SubPaneMarketplace => SubPaneMarketplaceShift,
        _ => throw new ArgumentException("unknown subpane", nameof(p)),
    };

    private void ApplySubPaneInstant(StackPanel target)
    {
        _subPaneAnimGen++;
        _subPaneAnimating = false;
        foreach (var p in new[] { SubPaneInstalledPlugins, SubPanePluginSecurity, SubPaneMarketplace })
        {
            p.Visibility = p == target ? Visibility.Visible : Visibility.Collapsed;
            p.Opacity = 1;
            SubPaneShiftOf(p).Y = 0;
        }
    }

    /// <summary>旧子面板淡出（120ms 上移 4px）→ 新子面板淡入（160ms 从下方 8px 升起）。</summary>
    private void StartSubPaneTransition(StackPanel current, StackPanel target)
    {
        var gen = ++_subPaneAnimGen;
        _subPaneAnimating = true;
        var curShift = SubPaneShiftOf(current);
        var tgtShift = SubPaneShiftOf(target);

        current.Opacity = 1;
        curShift.Y = 0;
        var outSb = new Storyboard();
        SubPaneFade(outSb, current, curShift, 1, 0, 0, -4, 120, new CubicEase { EasingMode = EasingMode.EaseIn });
        outSb.Completed += (_, _) =>
        {
            if (gen != _subPaneAnimGen) return;
            current.Visibility = Visibility.Collapsed;
            curShift.Y = 0;

            target.Visibility = Visibility.Visible;
            target.Opacity = 0;
            tgtShift.Y = 8;
            var inSb = new Storyboard();
            SubPaneFade(inSb, target, tgtShift, 0, 1, 8, 0, 160, new CubicEase { EasingMode = EasingMode.EaseOut });
            inSb.Completed += (_, _) =>
            {
                if (gen != _subPaneAnimGen) return;
                target.Opacity = 1;
                tgtShift.Y = 0;
                _subPaneAnimating = false;
            };
            inSb.Begin();
        };
        outSb.Begin();
    }

    private static void SubPaneFade(Storyboard sb, DependencyObject panel, DependencyObject shift,
        double op0, double op1, double y0, double y1, int ms, EasingFunctionBase ease)
    {
        var d = new Duration(TimeSpan.FromMilliseconds(ms));
        var aOp = new DoubleAnimation { From = op0, To = op1, Duration = d, EasingFunction = ease };
        Storyboard.SetTarget(aOp, panel);
        Storyboard.SetTargetProperty(aOp, "Opacity");
        sb.Children.Add(aOp);
        var aY = new DoubleAnimation { From = y0, To = y1, Duration = d, EasingFunction = ease };
        Storyboard.SetTarget(aY, shift);
        Storyboard.SetTargetProperty(aY, "Y");
        sb.Children.Add(aY);
    }

    private void SetPluginsTabButtonState(Button btn, bool active)
    {
        btn.Background = active
            ? (Brush)Root.Resources["AccentSoftBrush"]
            : new SolidColorBrush(Colors.Transparent);
        btn.Foreground = active
            ? (Brush)Root.Resources["TextPrimaryBrush"]
            : (Brush)Root.Resources["TextSecondaryBrush"];
        // 不加粗：SemiBold ↔ Normal 切换会让文字宽度变化抖动，统一用 Normal
        btn.FontWeight = Microsoft.UI.Text.FontWeights.Normal;
    }

    private async Task InitMarketplaceAsync()
    {
        try
        {
            var cfg = await _host.GetConfigAsync();
            PopulateMarketSourceCombo(cfg?.PluginRegistryUrls ?? new List<string>());
            _marketLoaded = true;
            await LoadMarketplaceAsync();
        }
        catch (Exception ex)
        {
            App.Log("InitMarketplace", ex);
            SetMarketStatus("初始化市场失败：" + ex.Message, isError: true);
        }
    }

    private void PopulateMarketSourceCombo(List<string> customUrls)
    {
        MarketSourceCombo.Items.Clear();
        MarketSourceCombo.Items.Add("官方仓库 (GitHub)");

        _customRepoUrls.Clear();
        foreach (var url in customUrls)
        {
            if (!string.IsNullOrWhiteSpace(url))
            {
                MarketSourceCombo.Items.Add(url.Trim());
                _customRepoUrls.Add(new CustomRepoUrlVm { Url = url.Trim() });
            }
        }
        MarketCustomUrlList.ItemsSource = _customRepoUrls;
        MarketSourceCombo.SelectedIndex = 0;
    }

    private void OnToggleManageCustomRepos(object sender, RoutedEventArgs e)
    {
        var vis = MarketCustomReposPanel.Visibility == Visibility.Visible;
        MarketCustomReposPanel.Visibility = vis ? Visibility.Collapsed : Visibility.Visible;
        MarketCustomManageBtn.Content = vis ? "管理仓库…" : "收起配置";
    }

    private void OnAddCustomRepoUrl(object sender, RoutedEventArgs e)
    {
        _customRepoUrls.Add(new CustomRepoUrlVm { Url = "https://" });
    }

    private void OnRemoveCustomRepoUrl(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is CustomRepoUrlVm item)
        {
            _customRepoUrls.Remove(item);
        }
    }

    private async void OnSaveCustomRepoUrls(object sender, RoutedEventArgs e)
    {
        try
        {
            var urls = _customRepoUrls
                .Select(x => x.Url.Trim())
                .Where(u => !string.IsNullOrEmpty(u) && (u.StartsWith("http://", StringComparison.OrdinalIgnoreCase) || u.StartsWith("https://", StringComparison.OrdinalIgnoreCase)))
                .Distinct(StringComparer.OrdinalIgnoreCase)
                .ToList();

            var update = new HostConfigUpdate { PluginRegistryUrls = urls };
            if (await _host.SetConfigAsync(update))
            {
                PopulateMarketSourceCombo(urls);
                MarketCustomReposPanel.Visibility = Visibility.Collapsed;
                MarketCustomManageBtn.Content = "管理仓库…";
                SetMarketStatus("仓库地址配置已保存", isError: false);
            }
            else
            {
                SetMarketStatus("保存仓库配置失败", isError: true);
            }
        }
        catch (Exception ex)
        {
            App.Log("SaveCustomRepoUrls", ex);
            SetMarketStatus("保存异常：" + ex.Message, isError: true);
        }
    }

    private async void OnMarketSourceChanged(object sender, SelectionChangedEventArgs e)
    {
        var idx = MarketSourceCombo.SelectedIndex;
        if (idx < 0) return;

        MarketCustomWarning.Visibility = idx > 0 ? Visibility.Visible : Visibility.Collapsed;
        await LoadMarketplaceAsync();
    }

    private async void OnMarketRefresh(object sender, RoutedEventArgs e)
    {
        await LoadMarketplaceAsync();
    }

    private void SetMarketStatus(string? text, bool isError = false)
    {
        MarketStatusText.Text = text ?? "";
        MarketStatusText.Foreground = isError
            ? new SolidColorBrush(Color.FromArgb(255, 255, 69, 58))
            : (Brush)Root.Resources["TextSecondaryBrush"];
        MarketStatusText.Visibility = string.IsNullOrEmpty(text) ? Visibility.Collapsed : Visibility.Visible;
    }

    private void OnMarketFilterChanged(object sender, object e)
    {
        // 全字段空引用短路：选中/文本事件可能在窗口初始化早期触发， 此时列表相关字段可能未就绪
        if (MarketFilterBox is null || MarketFilterCombo is null
            || MarketList is null || MarketFilterEmpty is null) return;
        ApplyMarketFilter();
    }

    /// <summary>
    /// 市场列表筛选：关键词 + 状态，仅内存过滤 _marketPlugins（数据源不动），ItemsSource 指向筛选副本。
    /// 安装成功回填写原 DTO（与筛选视图同一实例），按钮/角标状态照常原位刷新。
    /// 已知的瞬态不一致（accepted）：如"未安装"筛选激活时安装成功，卡片原位保留在已不再匹配的
    /// 视图里（显示"已是最新"），下次筛选变更/刷新自愈——重算 membership 会打断原位回填与退场动画。
    /// </summary>
    private void ApplyMarketFilter()
    {
        if (_marketPlugins.Count == 0)
        {
            MarketList.ItemsSource = null;
            MarketList.Visibility = Visibility.Collapsed;
            MarketFilterEmpty.Visibility = Visibility.Collapsed;
            return;
        }

        var kw = MarketFilterBox.Text?.Trim() ?? "";
        var view = new List<RegistryPluginViewDto>(_marketPlugins.Count);
        foreach (var it in _marketPlugins)
        {
            if (MatchesMarketFilter(it, kw, MarketFilterCombo.SelectedIndex)) view.Add(it);
        }

        // 筛选结果与当前视图逐项一致时跳过 ItemsSource 重赋值：整表替换会重建
        // 容器，打断安装中卡片的状态回填动画并重置滚动位置（逐键筛选的抖动源）。
        var skipReassign = false;
        if (MarketList.ItemsSource is List<RegistryPluginViewDto> cur && cur.Count == view.Count)
        {
            skipReassign = true;
            for (var i = 0; i < view.Count; i++)
            {
                if (!ReferenceEquals(cur[i], view[i])) { skipReassign = false; break; }
            }
        }
        if (!skipReassign) MarketList.ItemsSource = view;

        MarketList.Visibility = view.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        MarketFilterEmpty.Visibility = view.Count > 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>索引序与 XAML ComboBoxItem 一一对应：0全部 1未安装 2已安装 3官方 4已签名 5待签名 6签名失效。</summary>
    private static bool MatchesMarketFilter(RegistryPluginViewDto it, string keyword, int selectedIndex)
    {
        var stateOk = selectedIndex switch
        {
            1 => !it.IsInstalled,
            2 => it.IsInstalled,
            3 => it.DisplaySignState == PluginSignState.Official,
            4 => it.DisplaySignState == PluginSignState.ThirdParty,
            5 => it.PendingSignBadge,
            6 => it.DisplaySignState == PluginSignState.Invalid,
            _ => true,
        };
        if (!stateOk) return false;
        if (keyword.Length == 0) return true;
        return ContainsIgnoreCase(it.Name, keyword)
            || ContainsIgnoreCase(it.Description, keyword)
            || ContainsIgnoreCase(it.Author, keyword)
            || ContainsIgnoreCase(it.Id, keyword);
    }

    private static bool ContainsIgnoreCase(string? haystack, string needle)
        => haystack?.IndexOf(needle, StringComparison.OrdinalIgnoreCase) >= 0;

    private async Task LoadMarketplaceAsync()
    {
        if (_loadingMarket) return;
        _loadingMarket = true;

        MarketLoadingWrap.Visibility = Visibility.Visible;
        MarketEmpty.Visibility = Visibility.Collapsed;
        // 已有内容时保留旧列表可见（否则拉取索引期间整块消失，造成"闪一下"）；
        // 仅首次加载（空列表）才隐藏并显示 loading 占位
        if (_marketPlugins.Count == 0) MarketList.Visibility = Visibility.Collapsed;
        SetMarketStatus(null);

        var idx = MarketSourceCombo.SelectedIndex;
        var url = (idx > 0 && idx < MarketSourceCombo.Items.Count)
            ? MarketSourceCombo.Items[idx]?.ToString() ?? RegistryService.OfficialRegistryUrl
            : RegistryService.OfficialRegistryUrl;
        // 官方源门控：只有内置官方仓库的索引 signature 字段才参与"官方"预判，
        // 三方仓库的该字段可伪造（防钓鱼），不预判（规范 Phase 4.4）。
        var isOfficialSource = string.Equals(url, RegistryService.OfficialRegistryUrl, StringComparison.OrdinalIgnoreCase);

        try
        {
            var index = await RegistryService.FetchIndexAsync(url);
            _currentRegistry = index;
            var installed = await _host.PluginListAsync();

            _marketPlugins.Clear();

            foreach (var plugin in index.Plugins)
            {
                var targetVer = plugin.Versions.FirstOrDefault(v => v.Version == plugin.Latest)
                    ?? new RegistryVersionDto { Version = plugin.Latest, Path = $"{plugin.Id}/{plugin.Latest}" };

                var local = installed.FirstOrDefault(p => string.Equals(p.Id, plugin.Id, StringComparison.OrdinalIgnoreCase));
                var localVer = local?.Version;

                var canUpdate = false;
                var canDowngrade = false;
                if (!string.IsNullOrEmpty(localVer))
                {
                    var cmp = RegistryService.CompareVersion(targetVer.Version, localVer);
                    canUpdate = cmp > 0;
                    canDowngrade = cmp < 0;
                }

                _marketPlugins.Add(new RegistryPluginViewDto
                {
                    Plugin = plugin,
                    TargetVersion = targetVer,
                    InstalledVersion = localVer,
                    CanUpdate = canUpdate,
                    CanDowngrade = canDowngrade,
                    // 官方源门控 + 已装副本本地验签原文；卡片按 DisplaySignState 优先级展示角标。
                    IsOfficialSource = isOfficialSource,
                    InstalledSignStateRaw = local?.SignState,
                });
            }

            // 图标异步补齐：registry 的 icon 是 http(s) URL，下载+解码后回填；
            // 失败保持字母占位。列表刷新会整表重建 DTO，迟到结果落孤立对象无害。
            foreach (var it in _marketPlugins)
                _ = LoadMarketIconAsync(it);

            var empty = _marketPlugins.Count == 0;
            MarketEmpty.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
            MarketFilterRow.Visibility = empty ? Visibility.Collapsed : Visibility.Visible;
            ApplyMarketFilter();
        }
        catch (Exception ex)
        {
            App.Log("LoadMarketplace", ex);
            SetMarketStatus("无法连接仓库或解析索引：" + ex.Message, isError: true);
            // 抓取失败：清空残留索引与列表，避免用旧仓库元数据安装新仓库插件
            _currentRegistry = null;
            _marketPlugins.Clear();
            MarketFilterRow.Visibility = Visibility.Collapsed;
            ApplyMarketFilter();
            MarketList.Visibility = Visibility.Collapsed; // 与成功路径保持 MarketEmpty/MarketList 互斥不变量
            MarketEmpty.Visibility = Visibility.Visible;
        }
        finally
        {
            MarketLoadingWrap.Visibility = Visibility.Collapsed;
            _loadingMarket = false;
        }
    }

    /// <summary>市场卡片图标异步补齐：远程下载 + 解码（SVG/位图均支持），失败保持字母占位。</summary>
    private async Task LoadMarketIconAsync(RegistryPluginViewDto item)
    {
        try
        {
            var src = await PluginIconLoader.LoadRemoteAsync(item.Plugin.Icon);
            if (src is null) return;
            item.IconImage = src;
        }
        catch (Exception ex)
        {
            App.Log("MarketIcon", ex);
        }
    }

    private async void OnInstallFromMarketplace(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not RegistryPluginViewDto item) return;
        if (item.IsInstalling) return;
        var btn = sender as Button;

        // await 前置位防重入：原生确认框期间再点会被 IsInstalling 守卫挡住
        item.IsInstalling = true;
        item.InstallProgress = 0;
        StartInstallWave(btn);

        if (item.IsNative)
        {
            var confirmNative = await ConfirmDestructiveAsync(
                $"插件「{item.Name}」为原生插件 (Native)\n拥有操作系统完整执行权限。确认安装？");
            if (!confirmNative)
            {
                item.IsInstalling = false;
                StopInstallWave(btn);
                return;
            }
        }

        // 下载进度：HttpClient 线程池线程回调，1% 步进节流后经 DispatcherQueue 回 UI 线程。
        // 水位即按钮内水体高度：满格=下载完成，之后保持满格 + "安装中…"直到安装结果。
        // 容器回收会把 btn 重绑到别的条目：驱动视觉前校验 DataContext 仍是本条目，防止给错卡注水。
        var dispatcher = DispatcherQueue;
        double lastPct = -1;
        var progress = new Action<DownloadProgressReport>(rep =>
        {
            var pct = rep.Total is > 0 ? Math.Min(100.0, rep.Received * 100.0 / rep.Total.Value) : -1;
            if (pct >= 0 && pct < 100 && pct - lastPct < 1) return;
            lastPct = pct;
            dispatcher.TryEnqueue(() =>
            {
                item.ReportDownloadProgress(rep.Received, rep.Total);
                if (ReferenceEquals(btn?.DataContext, item))
                    SetInstallWaveLevel(btn, item.ProgressIndeterminate ? 45 : item.InstallProgress);
            });
        });

        string? tempDir = null;
        // 安装成功（含更新/降级）时为目标版本装机结果；确认取消/失败为 null（不回填卡片）
        PluginInstallOutcomeDto? done = null;
        try
        {
            if (!string.IsNullOrEmpty(item.TargetVersion.Url))
            {
                // 直链下载预打包 zip
                tempDir = await RegistryService.DownloadDirectZipAsync(
                    item.TargetVersion.Url,
                    item.TargetVersion.Sha256,
                    progress: progress);
            }
            else
            {
                // GitHub 仓库 zipball 提取模式：优先读索引声明的 zipball_url，回退官方地址。
                // 第三方仓库的 zipball_url 可能过期/写错，下载失败时自动回退官方常量重试一次。
                var zipballUrl = !string.IsNullOrWhiteSpace(_currentRegistry?.ZipballUrl)
                    ? _currentRegistry.ZipballUrl
                    : RegistryService.OfficialZipballUrl;
                var verPath = item.TargetVersion.Path ?? $"{item.Id}/{item.TargetVersion.Version}";

                try
                {
                    tempDir = await RegistryService.DownloadAndExtractZipballAsync(
                        zipballUrl,
                        verPath,
                        expectedSha256: null,
                        progress: progress);
                }
                catch (HttpRequestException ex) when (zipballUrl != RegistryService.OfficialZipballUrl)
                {
                    App.Log("InstallZipballFallback", ex);
                    lastPct = -1; // 回退地址是新的下载流，进度基线复位，避免条在旧值停滞
                    tempDir = await RegistryService.DownloadAndExtractZipballAsync(
                        RegistryService.OfficialZipballUrl,
                        verPath,
                        expectedSha256: null,
                        progress: progress);
                }
            }

            // 条满 = 下载完成；解压/安装阶段保持满格 + "安装中…"文案。
            item.ReportDownloadDone();
            if (ReferenceEquals(btn?.DataContext, item)) SetInstallWaveLevel(btn, 100);

            // 规范 Phase 4.4：registry signature 与包内 signature.json 装前比对（包内权威，仅记日志不阻断）。
            LogRegistrySignatureMismatch(item, tempDir);

            // 安装结果通过卡片原位回填呈现（按钮→已是最新、签名角标），状态栏不输出成功文案；
            // 仅失败时提示。
            var outcome = await _host.PluginInstallAsync(tempDir);
            switch (outcome.Action)
            {
                case "installed":
                    done = outcome;
                    break;
                case "updated":
                    done = outcome;
                    PluginWindowHost.CloseIfOpen(outcome.Id);
                    break;
                case "confirm_downgrade":
                {
                    var msg = $"检测到已安装较新版本\n当前已装 v{outcome.PreviousVersion}，是否覆盖降级安装 v{outcome.Version}？";
                    if (!await ConfirmDestructiveAsync(msg)) break;
                    PluginWindowHost.CloseIfOpen(outcome.Id);
                    done = await _host.PluginInstallAsync(tempDir, force: true);
                    break;
                }
                default:
                    // 未知动作不回填卡片（避免未来 host 新增拒绝/回滚类动作时被误标"已是最新"）
                    break;
            }

            await LoadPluginsAsync();
        }
        catch (Exception ex)
        {
            App.Log("InstallFromMarketplace", ex);
            SetMarketStatus($"安装 {item.Name} 失败：" + ex.Message, isError: true);
        }
        finally
        {
            RegistryService.CleanupTemp(tempDir);
            item.IsInstalling = false;
            // 原位回填卡片已装状态（不整表重建，列表不闪）：放在 IsInstalling=false 之后，
            // 让按钮文案/可用性直接落到终态（"已是最新"），排空动画不在"安装中"求值路径上播放。
            // 成功后同时清空状态栏（不输出成功文案）；失败信息（catch 所设）保留供用户读取
            if (done is not null)
            {
                item.UpdateInstalledState(done.Version, done.SignState);
                SetMarketStatus(null);
            }
            // 按钮可能已被容器回收重绑：只排空仍属于本条目的按钮（回收时已由
            // DataContextChanged/Unloaded 钩子复位过）
            if (ReferenceEquals(btn?.DataContext, item)) StopInstallWave(btn);
        }
    }

    // ==================== 市场安装按钮·水体波浪 ====================

    /// <summary>按钮 → 水体动画状态。按按钮隔离（多插件可并发安装，谁的动画谁停），
    /// 避免窗口级单槽被并发安装互相覆盖导致半途冻结/永久泄漏。</summary>
    private sealed class WaveState
    {
        /// <summary>双层波横流动画（Forever）。</summary>
        public Storyboard FlowSb = new();
        /// <summary>水位过渡动画（每次进度更新重定向，400ms 缓动）。</summary>
        public Storyboard? LevelSb;
        /// <summary>水体垂直位移：Y=按钮高（空，被裁剪不可见）→ 0（满格）。用位移而非 Height
        /// 驱动水位：RenderTransform 属独立动画（无需 EnableDependentAnimation）且不参与布局。</summary>
        public TranslateTransform WaterY = new();
        /// <summary>按钮高度基准（水位映射用）。容器未完成布局时可能为 0，由 SizeChanged 补齐。</summary>
        public double ButtonH;
        /// <summary>最近提交的水位百分比（0-100），供高度基准补齐时重建水位。</summary>
        public double LevelPct;
        /// <summary>当前水位动画的起止 Y / 起始时刻 / 时长——用于在重定向或排空前推算
        /// "此刻视觉水位"。WinUI 合成器动画不回写基值，WaterY.Y 始终是基值，不能直接读。</summary>
        public double LevelFromY;
        public double LevelToY;
        public DateTimeOffset LevelStartUtc;
        public TimeSpan LevelDuration = TimeSpan.Zero;
    }

    private readonly Dictionary<Button, WaveState> _waveStates = new();

    /// <summary>推算当前视觉水位 Y：水位过渡动画进行中时按缓动曲线插值，否则为基值。</summary>
    private static double CurrentVisualY(WaveState st)
    {
        if (st.LevelSb is null || st.LevelDuration.TotalMilliseconds <= 0) return st.WaterY.Y;
        var t = (DateTimeOffset.UtcNow - st.LevelStartUtc) / st.LevelDuration;
        t = Math.Clamp(t, 0, 1);
        var eased = 1 - Math.Pow(1 - t, 3); // CubicEase EaseOut
        return st.LevelFromY + (st.LevelToY - st.LevelFromY) * eased;
    }

    /// <summary>安装开始：铺满水体（初始整体沉到按钮下方不可见）、启动双层波横流。
    /// 水位由 SetInstallWaveLevel 以平滑过渡动画驱动；按钮自身即进度载体。</summary>
    private void StartInstallWave(Button? btn)
    {
        if (btn is null) return;
        var root = FindTemplateChild<Grid>(btn, "WaveRoot");
        var water = FindTemplateChild<Canvas>(btn, "WaveWater");
        var front = FindTemplateChild<XamlPath>(btn, "WaveFront");
        var back = FindTemplateChild<XamlPath>(btn, "WaveBack");
        if (root is null || water is null || front is null || back is null) return;

        // 同按钮重入（Loaded 重启/快速重装）先停旧动画，防累积
        if (_waveStates.TryGetValue(btn, out var existing))
        {
            existing.FlowSb.Stop();
            existing.LevelSb?.Stop();
            _waveStates.Remove(btn);
        }

        // 水体/波浪宽于按钮（264px 波形），用按钮矩形裁剪避免溢出卡片；裁剪随按钮尺寸
        // （安装文案变化会改宽）经 SizeChanged 持续刷新。WinUI 的 RectangleGeometry 无
        // RadiusX/Y；水体是半透明色，方形裁剪在 8px 圆角处差异不可见。
        root.SizeChanged -= OnWaveRootSizeChanged;
        root.SizeChanged += OnWaveRootSizeChanged;
        UpdateWaveClip(root);

        // 水体铺满按钮高，初始 Y=按钮高 → 整体沉到裁剪区外（空水）；水位上升 = Y 减小。
        // 容器刚 realize 尚未布局（滚动往返）时 ActualHeight=0：先登记状态，由
        // OnWaveRootSizeChanged 在首次有效测量时按 LevelPct 补建水位基准。
        var h = btn.ActualHeight;
        var waterY = new TranslateTransform { Y = h };
        water.RenderTransform = waterY;
        if (h > 0)
        {
            water.Height = h;
            water.Visibility = Visibility.Visible;
        }
        else
        {
            water.Height = 0;
            water.Visibility = Visibility.Collapsed;
        }

        var ttFront = new TranslateTransform();
        front.RenderTransform = ttFront;
        var ttBack = new TranslateTransform();
        back.RenderTransform = ttBack;
        var flowSb = new Storyboard();
        var daFront = new DoubleAnimation
        {
            From = 0,
            To = -24, // 一个波长；循环位移即"海面横流"
            Duration = new Duration(TimeSpan.FromMilliseconds(1600)),
            RepeatBehavior = RepeatBehavior.Forever,
        };
        Storyboard.SetTarget(daFront, ttFront);
        Storyboard.SetTargetProperty(daFront, "X");
        var daBack = new DoubleAnimation
        {
            From = -24,
            To = 0,
            Duration = new Duration(TimeSpan.FromMilliseconds(2400)),
            RepeatBehavior = RepeatBehavior.Forever,
        };
        Storyboard.SetTarget(daBack, ttBack);
        Storyboard.SetTargetProperty(daBack, "X");
        flowSb.Children.Add(daFront);
        flowSb.Children.Add(daBack);
        flowSb.Begin();

        _waveStates[btn] = new WaveState
        {
            FlowSb = flowSb,
            WaterY = waterY,
            ButtonH = h,
            LevelPct = 0,
            LevelFromY = h,
            LevelToY = h,
        };
    }

    /// <summary>水位更新（UI 线程）：pct 0-100 → 从当前视觉水位以 400ms 缓动动画平滑推向目标。
    /// 小插件秒下也能看到一段完整的上升过程；频繁更新则逐次重定向，不跳变、不塌空。</summary>
    private void SetInstallWaveLevel(Button? btn, double pct)
    {
        if (btn is null || !_waveStates.TryGetValue(btn, out var st)) return;
        if (st.ButtonH <= 0)
        {
            // 高度基准未就绪（容器未布局）：只记账，等 SizeChanged 补建基准
            st.LevelPct = Math.Clamp(pct, 0, 100);
            return;
        }
        var currentY = CurrentVisualY(st);
        // 关键：先把基值固化为当前视觉水位，再停旧动画——WinUI 合成器动画不回写
        // WaterY.Y，Stop 会让视觉塌回基值，导致"清空-重爬"抖动
        st.WaterY.Y = currentY;
        st.LevelSb?.Stop();
        st.LevelPct = Math.Clamp(pct, 0, 100);
        st.LevelFromY = currentY;
        st.LevelToY = st.ButtonH * (1 - st.LevelPct / 100.0);
        st.LevelStartUtc = DateTimeOffset.UtcNow;
        st.LevelDuration = TimeSpan.FromMilliseconds(400);
        var da = new DoubleAnimation
        {
            From = currentY,
            To = st.LevelToY,
            Duration = new Duration(st.LevelDuration),
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
        };
        Storyboard.SetTarget(da, st.WaterY);
        Storyboard.SetTargetProperty(da, "Y");
        var sb = new Storyboard();
        sb.Children.Add(da);
        sb.Begin();
        st.LevelSb = sb;
    }

    private void OnWaveRootSizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (sender is not Grid g) return;
        UpdateWaveClip(g);
        // 高度基准补齐：Start 时 ActualHeight=0（容器未布局）的场景——首次有效测量时
        // 按 LevelPct 重建水位基准（直接落位不动画），后续进度更新再走平滑动画
        if (g.ActualHeight <= 0) return;
        DependencyObject? p = g;
        while (p is not null && p is not Button) p = VisualTreeHelper.GetParent(p);
        if (p is not Button b || !_waveStates.TryGetValue(b, out var st) || st.ButtonH > 0) return;
        var h = b.ActualHeight;
        if (h <= 0) return;
        st.ButtonH = h;
        st.LevelToY = h * (1 - Math.Clamp(st.LevelPct, 0, 100) / 100.0);
        st.WaterY.Y = st.LevelToY;
        st.LevelSb?.Stop();
        st.LevelSb = null; // 基值已固化，旧动画作废；后续进度从当前水位继续过渡
        var water = FindTemplateChild<Canvas>(b, "WaveWater");
        if (water is not null)
        {
            water.Height = h;
            water.Visibility = Visibility.Visible;
        }
    }

    private static void UpdateWaveClip(Grid root)
    {
        if (root.ActualWidth > 0 && root.ActualHeight > 0)
        {
            root.Clip = new RectangleGeometry { Rect = new Rect(0, 0, root.ActualWidth, root.ActualHeight) };
        }
    }

    /// <summary>停止指定按钮的水体波浪：停横流/水位动画，水位以 250ms 排空动画退场后隐藏。
    /// 只动传入按钮自己的动画（按按钮隔离）。无状态（回收残留兜底）时直接隐藏。</summary>
    private void StopInstallWave(Button? btn)
    {
        if (btn is null) return;
        var water = FindTemplateChild<Canvas>(btn, "WaveWater");
        if (_waveStates.TryGetValue(btn, out var st))
        {
            // 先固化当前视觉水位为基值再停动画（否则视觉塌回基值，排空不可见）
            var currentY = CurrentVisualY(st);
            st.WaterY.Y = currentY;
            st.FlowSb.Stop();
            st.LevelSb?.Stop();
            _waveStates.Remove(btn);

            if (water is not null && st.ButtonH > 0)
            {
                // 排空过渡；期间按钮若重新开始安装（新水体已挂新 Transform），
                // Completed 按 Transform 身份判定，不误伤新水位
                var da = new DoubleAnimation
                {
                    From = currentY,
                    To = st.ButtonH,
                    Duration = new Duration(TimeSpan.FromMilliseconds(250)),
                };
                Storyboard.SetTarget(da, st.WaterY);
                Storyboard.SetTargetProperty(da, "Y");
                var sb = new Storyboard();
                sb.Children.Add(da);
                sb.Completed += (_, _) =>
                {
                    var w = FindTemplateChild<Canvas>(btn, "WaveWater");
                    if (w is not null && ReferenceEquals(w.RenderTransform, st.WaterY))
                    {
                        w.Height = 0;
                        w.RenderTransform = null;
                        w.Visibility = Visibility.Collapsed;
                    }
                };
                sb.Begin();
            }
            else if (water is not null)
            {
                water.Height = 0;
                water.RenderTransform = null;
                water.Visibility = Visibility.Collapsed;
            }
        }
        else if (water is not null)
        {
            water.Height = 0;
            water.RenderTransform = null;
            water.Visibility = Visibility.Collapsed;
        }
    }

    // 容器回收/滚动/整表重建会把按钮重绑到别的条目或移出可视树：必须排空水体并停掉
    // 本按钮的动画，防止上一条目的水位/波浪残留在无关插件的卡片上。重新进入视口时，
    // 若该条目仍在安装中，Loaded 按当前进度重启波浪。
    private void OnInstallButtonUnloaded(object sender, RoutedEventArgs e)
        => StopInstallWave(sender as Button);

    private void OnInstallButtonLoaded(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn) return;
        if (btn.DataContext is not RegistryPluginViewDto item || !item.IsInstalling)
        {
            StopInstallWave(btn); // 非安装态一律排空（清回收残留）
            return;
        }
        StartInstallWave(btn);
        SetInstallWaveLevel(btn, item.ProgressIndeterminate ? 45 : item.InstallProgress);
    }

    private void OnInstallButtonDataContextChanged(FrameworkElement sender, DataContextChangedEventArgs args)
    {
        if (sender is not Button btn) return;
        // 重绑即复位：回收容器可能带着上一条目的水位与运行中动画
        // （WinUI 3 的 DataContextChangedEventArgs 不带 NewData，直接读 sender.DataContext）
        StopInstallWave(btn);
        if (btn.DataContext is RegistryPluginViewDto item && item.IsInstalling)
        {
            StartInstallWave(btn);
            SetInstallWaveLevel(btn, item.ProgressIndeterminate ? 45 : item.InstallProgress);
        }
    }

    /// <summary>视觉树递归找模板内命名子元素（Template.FindName 在 WinUI 对模板件不可靠，走视觉树更稳）。</summary>
    private static T? FindTemplateChild<T>(DependencyObject root, string name) where T : FrameworkElement
    {
        int count = VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < count; i++)
        {
            if (VisualTreeHelper.GetChild(root, i) is not FrameworkElement fe) continue;
            if (fe.Name == name && fe is T match) return match;
            var deep = FindTemplateChild<T>(fe, name);
            if (deep is not null) return deep;
        }
        return null;
    }

    /// <summary>装前预检（规范 Phase 4.4）：registry 索引声明的 signature 与包内 signature.json
    /// 比对，不一致仅记日志（包内权威，以包内为准；host 安装时仍会全量验签兜底）。
    /// 包内缺 signature.json 不在此报——由 host 安装验签按策略拦截/记录。</summary>
    private static void LogRegistrySignatureMismatch(RegistryPluginViewDto item, string dir)
    {
        var regSig = item.TargetVersion.Signature;
        if (regSig is null) return;
        try
        {
            var pkgPath = Path.Combine(dir, "signature.json");
            if (!File.Exists(pkgPath)) return;
            using var doc = JsonDocument.Parse(File.ReadAllText(pkgPath));
            var root = doc.RootElement;
            var pkgSig = root.TryGetProperty("signature", out var sigProp) ? sigProp.GetString() : null;
            var pkgKeyId = root.TryGetProperty("key_id", out var keyProp) ? keyProp.GetString() : null;
            if (!string.Equals(pkgSig?.Trim(), regSig.Signature.Trim(), StringComparison.Ordinal)
                || !string.Equals(pkgKeyId, regSig.KeyId, StringComparison.Ordinal))
            {
                App.Log("RegistrySignature", new InvalidDataException(
                    $"{item.Id}@{item.TargetVersion.Version}: registry signature 与包内不一致（以包内为准，建议同步修正 registry）"));
            }
        }
        catch (Exception ex)
        {
            // 预检自身失败（signature.json 损坏等）不阻断安装，host 装时全量验签兜底。
            App.Log("RegistrySignature", ex);
        }
    }

    // ==================== 关于 ====================

    private const string GithubRepo = "https://github.com/MrHan-Yd/spark";
    /// <summary>发布清单：GitHub Release 最新版的 latest.json 资产（latest/download 重定向，CDN 直取、不走 API）。</summary>
    private const string UpdateManifestUrl = GithubRepo + "/releases/latest/download/latest.json";
    private bool _checkingUpdate;
    private UpdateManifest? _pendingUpdate;

    /// <summary>当前应用版本（csproj Version，与 Cargo 工作区版本保持一致）。</summary>
    private static Version AppVersion
        => Assembly.GetExecutingAssembly().GetName().Version ?? new Version(0, 1, 0);

    private static string AppVersionText => AppVersion.ToString(3);

    /// <summary>更新清单：版本号 + 安装包地址 + SHA-256。</summary>
    private sealed record UpdateManifest(string Version, string Url, string Sha256);

    private void OnOpenGithub(object sender, RoutedEventArgs e) => OpenUrl(GithubRepo);

    private void OnOpenRelease(object sender, RoutedEventArgs e) => OpenUrl(GithubRepo + "/releases");

    private static void OpenUrl(string url)
    {
        try { Process.Start(new ProcessStartInfo(url) { UseShellExecute = true }); }
        catch (Exception ex) { App.Log("OpenUrl", ex); }
    }

    /// <summary>检查更新：拉取发布清单（latest.json）并与本地版本比较；
    /// 发现新版本后按钮转为"下载并安装"（再点一次进入下载安装流程）。
    /// 未发布过 Release（404）或网络失败都只更新状态文字，不弹窗打断。</summary>
    private async void OnCheckUpdate(object sender, RoutedEventArgs e)
    {
        if (_checkingUpdate) return;
        if (_pendingUpdate is not null)
        {
            await DownloadAndInstallAsync(_pendingUpdate);
            return;
        }
        _checkingUpdate = true;
        BtnCheckUpdate.IsEnabled = false;
        CheckUpdateLabel.Text = "检查中…";
        BtnOpenRelease.Visibility = Visibility.Collapsed;
        AboutUpdateStatus.Text = "正在检查更新…";
        try
        {
            // 检查更新走代理回退链：与市场索引同一套网络容错（默认客户端 → 环境代理客户端）
            var json = await RegistryService.GetStringWithProxyFallbackAsync(UpdateManifestUrl);
            var m = ParseManifest(json);
            if (m is null)
            {
                AboutUpdateStatus.Text = "检查失败，请稍后重试";
                return;
            }
            var cmp = ParseVersion(m.Version).CompareTo(ParseVersion(AppVersionText));
            if (cmp > 0)
            {
                _pendingUpdate = m;
                CheckUpdateLabel.Text = "下载并安装";
                AboutUpdateStatus.Text = $"发现新版本 {m.Version}";
                BtnOpenRelease.Visibility = Visibility.Visible;
            }
            else if (cmp == 0)
            {
                AboutUpdateStatus.Text = "已是最新版本";
            }
            else
            {
                AboutUpdateStatus.Text = $"本地版本更新于远端（{m.Version}）";
            }
        }
        catch (HttpRequestException ex) when (ex.StatusCode == System.Net.HttpStatusCode.NotFound)
        {
            AboutUpdateStatus.Text = "暂无发布版本";
        }
        catch (Exception ex)
        {
            AboutUpdateStatus.Text = "检查失败，请稍后重试";
            App.Log("CheckUpdate", ex);
        }
        finally
        {
            _checkingUpdate = false;
            BtnCheckUpdate.IsEnabled = true;
            if (_pendingUpdate is null) CheckUpdateLabel.Text = "检查更新";
        }
    }

    /// <summary>下载 → 校验 SHA-256 → 静默安装 → 杀旧 host → 拉起新版 host/UI → 退出。
    /// 安装器会强制关闭运行中的 Spark.exe / spark-host.exe（CloseApplications=force，Restart Manager
    /// 检测，仅对占用安装目录待更新文件的进程生效），装完 [Run] 拉起新版 host；但对"运行副本不在
    /// 安装目录"的场景（开发副本等）旧 host 杀不到，由本方法在安装成功后显式补杀旧 host（释放
    /// 单实例互斥体）再拉起新版 host/UI，完成"自动重启"（详见 <see cref="KillOldHostAsync"/>）。
    /// 安装成功前不动旧 host —— 校验不过/安装失败时环境完好，仅保留"打开下载页"手动兜底。
    /// 弱网（GitHub 大文件易中断/损坏）下：下载中断自动重试（断点续传，不必全量重下），
    /// 校验失败同样自动重下重试，最多 <see cref="MaxDownloadAttempts"/> 次。</summary>
    private const int MaxDownloadAttempts = 3;

    private async Task DownloadAndInstallAsync(UpdateManifest m)
    {
        _checkingUpdate = true;
        BtnCheckUpdate.IsEnabled = false;
        CheckUpdateLabel.Text = "下载中…";
        UpdateProgress.Visibility = Visibility.Visible;
        UpdateProgress.Value = 0;
        var dir = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Spark", "update");
        Directory.CreateDirectory(dir);
        var setup = Path.Combine(dir, $"Spark-{m.Version}-setup.exe");
        try
        {
            using var clientDefault = new HttpClient();
            clientDefault.DefaultRequestHeaders.UserAgent.ParseAdd("Spark/" + AppVersionText);
            var envProxyClient = RegistryService.EnvProxyClient;

            var verified = false;
            var everDownloaded = false;
            for (var attempt = 1; attempt <= MaxDownloadAttempts && !verified; attempt++)
            {
                // 弱网兜底：偶数次尝试改走环境变量代理客户端（系统代理在当前进程上下文可能失效）
                var client = envProxyClient is not null && attempt % 2 == 0 ? envProxyClient : clientDefault;
                var downloaded = await TryDownloadAsync(client, m.Url, setup);
                if (!downloaded)
                {
                    if (attempt < MaxDownloadAttempts)
                    {
                        AboutUpdateStatus.Text = $"下载中断（第 {attempt} 次），稍后自动重试…";
                        await Task.Delay(1500);
                    }
                    continue;
                }
                everDownloaded = true;

                // 校验 SHA-256：不一致不安装
                AboutUpdateStatus.Text = "正在校验…";
                string hash;
                await using (var fs = File.OpenRead(setup))
                {
                    hash = Convert.ToHexString(await SHA256.HashDataAsync(fs));
                }
                if (string.Equals(hash, m.Sha256, StringComparison.OrdinalIgnoreCase))
                {
                    verified = true;
                    break;
                }

                // 内容损坏：删掉重下（断点续传对坏文件无意义）
                File.Delete(setup);
                if (attempt < MaxDownloadAttempts)
                {
                    AboutUpdateStatus.Text =
                        $"校验失败（期望 {ShortHash(m.Sha256)}，实际 {ShortHash(hash)}），第 {attempt} 次，重新下载…";
                    await Task.Delay(1500);
                }
            }

            if (!verified)
            {
                // 一次都没下载成功（网络/416 恢复失败）与"下载成功但校验不过"是两回事，提示分开
                AboutUpdateStatus.Text = everDownloaded
                    ? $"校验失败，请稍后重试（期望 {ShortHash(m.Sha256)}）"
                    : "下载失败，请检查网络后重试";
                return;
            }

            // 静默安装到原安装路径（注册表记录；读不到则默认目录）
            CheckUpdateLabel.Text = "正在安装…";
            AboutUpdateStatus.Text = "正在安装，完成后将自动重启…";
            var installDir = ReadInstallPath() ?? "";
            // 注册表 InstallPath 可能被写坏（空串/相对路径/带引号），兜底到默认目录，
            // 避免 /DIR 参数损坏导致装错位置
            installDir = installDir.Trim().Trim('"');
            if (!Path.IsPathRooted(installDir))
            {
                installDir = Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                    "Programs", "Spark");
            }
            var psi = new ProcessStartInfo(setup)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                Arguments = $"/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /DIR=\"{installDir}\"",
            };
            using var proc = Process.Start(psi);
            if (proc is null)
            {
                AboutUpdateStatus.Text = "安装启动失败，请手动下载";
                return;
            }
            await proc.WaitForExitAsync();
            if (proc.ExitCode != 0)
            {
                // 安装失败：此时旧 host 未动（成功前绝不杀它），热键/托盘/索引环境完好
                AboutUpdateStatus.Text = $"安装失败（{proc.ExitCode}），请手动下载";
                return;
            }
            // 安装成功。旧 host 可能还活着（不在安装目录的实例，CloseApplications 杀不到，见
            // KillOldHostAsync）——先杀掉释放单实例互斥体，再显式拉起新版 host/UI 完成"自动重启"。
            // 重启步骤成败可观测：旧 host 没杀干净（互斥体仍被占）或新版 host 没起来时不能静默
            // 退出（否则留下"无 host/旧 host"残缺态），保留本进程并提示用户手动重启应用。
            if (!await KillOldHostAsync() || !LaunchNewHost(installDir))
            {
                AboutUpdateStatus.Text = "更新完成，请手动重启应用";
                return;
            }
            // 新版 host 已接管（安装期间 [Run] 拉起的 host 实例若还活着，也随上面的 taskkill
            // 一并结束，最终恰好一个 host）。UI 由本进程补拉：host 启动时检测到本进程
            // （Spark.exe）在运行，不会自己拉起 UI；补拉失败也无妨，下次热键/托盘唤起时
            // host 会自行拉起新版 UI（自愈）。开发副本场景安装期间，[Run] 的 host 实例向
            // 旧 host 转发过一次 toggle，更新窗口可能被显隐一次 —— 纯外观副作用。
            LaunchNewUi(installDir);
            Environment.Exit(0);
        }
        catch (Exception ex)
        {
            AboutUpdateStatus.Text = "更新失败，请稍后重试";
            App.Log("Update", ex);
        }
        finally
        {
            UpdateProgress.Visibility = Visibility.Collapsed;
            _checkingUpdate = false;
            BtnCheckUpdate.IsEnabled = true;
            CheckUpdateLabel.Text = _pendingUpdate is null ? "检查更新" : "下载并安装";
        }
    }

    /// <summary>强杀运行中的旧 host（taskkill 按映像名，与安装位置无关；host 不在运行时空跑无害）。
    /// 为什么必须显式杀：安装器的 CloseApplications=force 走 Windows Restart Manager，只关闭
    /// "占用安装目录待更新文件"的进程 —— 当正在运行的 spark-host.exe 不在安装目录（开发副本、
    /// 手动拷贝的运行位置）时检测不到，旧 host 会带着单实例互斥体/热键/托盘活过安装，
    /// [Run] 拉起的 host 因"实例已存在"立即退出并转发 toggle，之后所有唤醒仍由旧 host 提供，
    /// 表现为"更新成功但唤醒后还是旧版本"（v0.2.9 实测）。
    /// 不用 spark-host --exit：它走 host.exit 广播退出事件，会把正在执行更新的本 UI 一起关掉。
    /// 不带 /T：host 可能以本 UI 为子进程（host 拉起 UI），带 /T 会把更新流程自己杀死。
    /// 注意：按映像名杀会把安装期间 [Run] 刚拉起的 host 实例一并结束（它也是 spark-host.exe），
    /// 由调用方随后显式拉起新版 host，最终恰好一个实例。
    /// 返回 false 表示确认仍有 spark-host 进程存活（taskkill 被拦截等）——单实例互斥体未释放，
    /// 调用方必须保留本进程并提示用户，不能静默退出。</summary>
    private static async Task<bool> KillOldHostAsync()
    {
        try
        {
            var psi = new ProcessStartInfo("taskkill")
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                Arguments = "/IM spark-host.exe /F",
            };
            using var p = Process.Start(psi);
            if (p is not null)
            {
                using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(3));
                try { await p.WaitForExitAsync(cts.Token); } catch (OperationCanceledException) { }
            }
            // taskkill 返回只代表终止请求已发出，再等进程真正退出 —— 单实例互斥体随进程
            // 结束才释放，新版 host 必须在互斥体空闲时启动（否则又退回旧 host 兜底）。
            for (var i = 0; i < 20 && Process.GetProcessesByName("spark-host").Length > 0; i++)
                await Task.Delay(100);
            var stillRunning = Process.GetProcessesByName("spark-host").Length > 0;
            if (stillRunning)
                App.Log("KillOldHost", "taskkill 未生效，spark-host 仍在运行 — 更新后可能仍回退旧版本");
            return !stillRunning;
        }
        catch (Exception ex)
        {
            App.Log("KillOldHost", ex);
            return false;
        }
    }

    /// <summary>安装成功后显式拉起新版 host。安装器 [Run] 已尝试过拉起；[Run] 的实例若因旧 host
    /// 占互斥体而退出（开发副本场景），这里补拉；若旧 host 杀不掉，本次启动会走"实例已存在→
    /// 转发 toggle"后退出。host 文件不存在或启动被拦截（杀软等）时返回 false。
    /// host 启动时若检测到 Spark.exe 在运行（本更新进程），不会自动拉起 UI，需配合
    /// <see cref="LaunchNewUi"/>。</summary>
    private static bool LaunchNewHost(string installDir)
    {
        try
        {
            var host = Path.Combine(installDir, "spark-host.exe");
            if (!File.Exists(host)) return false;
            var psi = new ProcessStartInfo(host)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                WorkingDirectory = installDir,
            };
            return Process.Start(psi) is not null;
        }
        catch (Exception ex)
        {
            App.Log("LaunchNewHost", ex);
            return false;
        }
    }

    /// <summary>安装成功后显式拉起新版 UI（--hidden，与 host 启动同款参数），补全"自动重启"：
    /// [Run] 的 host 启动时本更新进程还活着，host 的"UI 已在运行"检查（tasklist 按映像名）
    /// 会跳过拉起，本进程退出后就没人拉 UI 了 —— 由更新发起者自己补拉。
    /// 时序安全：调用后紧接着 Environment.Exit(0)（毫秒级），新版 UI 冷启动（数百毫秒）
    /// 远慢于本进程退出，届时 UI 单实例互斥体（Local\SparkUISingleInstance_v1）已随进程
    /// 结束释放，不会误入"已有实例→转发 toggle→退出"分支。文件不存在时静默放弃。</summary>
    private static void LaunchNewUi(string installDir)
    {
        try
        {
            var ui = Path.Combine(installDir, "Spark.exe");
            if (!File.Exists(ui)) return;
            var psi = new ProcessStartInfo(ui)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                Arguments = "--hidden",
                WorkingDirectory = installDir,
            };
            Process.Start(psi);
        }
        catch (Exception ex)
        {
            App.Log("LaunchNewUi", ex);
        }
    }

    /// <summary>流式下载到 setup 文件（带进度）；已存在部分文件时发 Range 续传。
    /// 中断/失败返回 false（由调用方决定重试），不抛异常。
    /// 服务端回 416（本地残留长度 ≥ 远端文件：上次已下满但校验/安装中断，或远端重新发布）
    /// 时删除残留、去掉 Range 全量重下 —— 否则每次重试都撞同一个 416 死循环。</summary>
    private async Task<bool> TryDownloadAsync(HttpClient client, string url, string file)
    {
        try
        {
            var done = File.Exists(file) ? new FileInfo(file).Length : 0;
            for (var fresh = true; ; fresh = false)
            {
                using var req = new HttpRequestMessage(HttpMethod.Get, url);
                if (done > 0)
                    req.Headers.Range = new RangeHeaderValue(done, null);
                using var resp = await client.SendAsync(req, HttpCompletionOption.ResponseHeadersRead);
                if (resp.StatusCode == HttpStatusCode.RequestedRangeNotSatisfiable)
                {
                    // 416：残留文件已无续传价值，删掉从头下；仍 416（第二次）就放弃
                    if (!fresh) return false;
                    File.Delete(file);
                    done = 0;
                    continue;
                }
                resp.EnsureSuccessStatusCode();
                // 服务器忽略 Range（200 全量）→ 从头重写；206 才追加续传
                var append = resp.StatusCode == HttpStatusCode.PartialContent;
                var remaining = resp.Content.Headers.ContentLength ?? 0;
                await using var src = await resp.Content.ReadAsStreamAsync();
                await using var dst = append
                    ? new FileStream(file, FileMode.Append, FileAccess.Write, FileShare.None)
                    : File.Create(file);
                var buf = new byte[64 * 1024];
                long total = append ? done + remaining : remaining;
                while (true)
                {
                    var n = await src.ReadAsync(buf);
                    if (n <= 0) break;
                    await dst.WriteAsync(buf.AsMemory(0, n));
                    done += n;
                    if (total > 0)
                    {
                        var pct = (int)(done * 100 / total);
                        UpdateProgress.Value = pct;
                        AboutUpdateStatus.Text = $"正在下载 {pct}%（{done / 1048576} MB）";
                    }
                }
                return true;
            }
        }
        catch (Exception ex)
        {
            App.Log("UpdateDownload", ex);
            return false;
        }
    }

    /// <summary>sha256 摘要取前 8 位用于错误提示（完整串太长）。</summary>
    private static string ShortHash(string s) => s is { Length: > 8 } ? s[..8] : s ?? "";

    /// <summary>读注册表里安装器记录的安装路径（HKCU\Software\Spark\InstallPath）。</summary>
    private static string? ReadInstallPath()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(@"Software\Spark");
            return key?.GetValue("InstallPath") as string;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>解析 latest.json 发布清单；字段缺失或 JSON 损坏返回 null。</summary>
    private static UpdateManifest? ParseManifest(string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var r = doc.RootElement;
            var v = r.TryGetProperty("version", out var t) ? t.GetString() : null;
            var u = r.TryGetProperty("url", out var x) ? x.GetString() : null;
            var h = r.TryGetProperty("sha256", out var y) ? y.GetString() : null;
            return string.IsNullOrEmpty(v) || string.IsNullOrEmpty(u) || string.IsNullOrEmpty(h)
                ? null
                : new UpdateManifest(v, u, h);
        }
        catch (JsonException)
        {
            return null;
        }
    }

    /// <summary>解析 "v0.2.0" 这类版本串为可比较三元组（缺段按 0）。</summary>
    private static (int, int, int) ParseVersion(string s)
    {
        var p = s.TrimStart('v', 'V').Split('.');
        int Get(int i) => i < p.Length && int.TryParse(p[i], out var n) ? n : 0;
        return (Get(0), Get(1), Get(2));
    }

    // ==================== 键盘 / 执行 ====================

    private void OnQueryChanged(object sender, TextChangedEventArgs e)
        => ScheduleRefresh(QueryBox.Text ?? "");

    /// <summary>查询调度：连续输入防抖合并；IME 组词期间挂起，
    /// 组词结束（TextCompositionEnded）再补查；首字符（空→非空）立即查询不走防抖。</summary>
    private void ScheduleRefresh(string q)
    {
        _debounceCts?.Cancel();
        if (_composing) return;
        var immediate = _lastScheduledQuery.Trim().Length == 0 && q.Trim().Length > 0;
        _lastScheduledQuery = q;
        _debounceCts = new CancellationTokenSource();
        var ct = _debounceCts.Token;
        _ = RefreshAfterDebounceAsync(q, ct, immediate ? 0 : QueryDebounceMs);
    }

    private async Task RefreshAfterDebounceAsync(string q, CancellationToken ct, int delayMs)
    {
        try { await Task.Delay(delayMs, ct); }
        catch (TaskCanceledException) { return; }
        if (ct.IsCancellationRequested) return;
        await RefreshResultsAsync(q);
    }

    private async void OnQueryKeyDown(object sender, KeyRoutedEventArgs e)
    {
        var ctrl = Microsoft.UI.Input.InputKeyboardSource
            .GetKeyStateForCurrentThread(VirtualKey.Control)
            .HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down);

        // Ctrl+, → 设置（VK 0xBC = OEM comma）
        if (ctrl && (int)e.Key == 0xBC)
        {
            e.Handled = true;
            OnOpenSettings(sender, e);
            return;
        }

        if (e.Key == VirtualKey.Enter)
        {
            e.Handled = true;
            await InvokeAsync();
            return;
        }
    }

    /// <summary>根上 handledEventsToo 收箭头键（冒泡终点）：输入框有文本时 TextBox 已先消费箭头（光标移动），
    /// 这里仍能收到，统一只控制选中项，并把输入框光标拉回末尾（搜索框只能输入/退格，不能移光标）。
    /// 列表/平铺/收藏区都走这里：列表视图左右等价上一条/下一条，平铺按网格行列移动
    /// （直上直下：正下方没有元素不跳到最后）；结果区末尾继续往下进入收藏区，
    /// 收藏区首位继续往上回到结果区（_favActive 跟踪）。
    /// 焦点在收藏坞交互件上（点了卡片/分组）时也纳入收藏区导航，不交给原生方向键。
    /// 注意：焦点在条目上时原生 ListView/GridView 会先于本处理器移动选中（键盘光标跟随），
    /// 因此这里从 _active（按下前的值）计算目标并绝对赋值；若读 SelectedIndex 重算会叠加上原生那一步，造成双重移动。</summary>
    private void OnRootKeyDown(object sender, KeyRoutedEventArgs e)
    {
        // 模态面板打开时独占按键：Esc 关闭（新建分组/删除确认），其余键交给面板内控件
        if (FavGroupPanel.Visibility == Visibility.Visible
            || FavConfirmPanel.Visibility == Visibility.Visible)
        {
            if (e.Key == VirtualKey.Escape)
            {
                e.Handled = true;
                if (FavConfirmPanel.Visibility == Visibility.Visible)
                    CloseFavConfirmPanel();
                else
                    CloseFavGroupPanel();
            }
            return;
        }
        if (_composing) return;                       // IME 组词中不拦截
        if (MainPanel.Visibility != Visibility.Visible)
        {
            // 主页隐藏时（设置页打开）：只放行 Esc 关闭设置 —— 焦点可能还停在搜索框上，
            // 设置页自身的 KeyDown（OnSettingsKeyDown）收不到，需要根上兜底
            if (e.Key == VirtualKey.Escape && SettingsPanel.Visibility == Visibility.Visible)
            {
                e.Handled = true;
                OnCloseSettings(sender, e);
            }
            return;
        }
        if (_itemMenu?.IsOpen == true) return;        // 动作菜单打开中：方向键/回车交给菜单

        if (e.Key == VirtualKey.Escape)
        {
            // Esc 在根上统一处理（焦点可能在搜索框/结果/收藏任意位置）
            e.Handled = true;
            if (SettingsPanel.Visibility == Visibility.Visible)
            {
                OnCloseSettings(sender, e);
                return;
            }
            if (!string.IsNullOrEmpty(QueryBox.Text))
            {
                // 清空会触发 OnQueryChanged → ScheduleRefresh，不需要再手动刷
                QueryBox.Text = "";
                return;
            }
            HideLauncher();
            return;
        }
        if (e.Key == VirtualKey.Tab)
        {
            // Tab 动作（对齐原型 action-sheet）：对当前选中项弹出动作菜单
            e.Handled = true;
            if (_favActive < 0) ShowActiveItemMenu();
            return;
        }
        if (e.Key is not (VirtualKey.Down or VirtualKey.Up or VirtualKey.Left or VirtualKey.Right)) return;

        e.Handled = true;
        QueryBox.SelectionStart = QueryBox.Text.Length;
        QueryBox.SelectionLength = 0;
        // 鼠标点过收藏坞（焦点落在交互件上）且尚未进入收藏区导航 → 从当前卡片进入
        if (IsFocusOnFavorites() && _favActive < 0)
            _favActive = 0;
        MoveSelection(e.Key);
    }

    /// <summary>方向键移动选中：结果区 ↔ 收藏区无缝衔接。
    /// 平铺视图"直上直下"：正下方没有元素就停在原地，不跳到最后；
    /// 结果区末尾继续往下进入收藏区，收藏区首位继续往上回到结果区。</summary>
    private void MoveSelection(VirtualKey key)
    {
        var favCount = FavBodyClip.Visibility == Visibility.Visible ? FavButtons().Count : 0;

        // 收藏区：一行卡片，右/下 = 下一张，左/上 = 上一张，首位再往上是回结果区
        if (_favActive >= 0)
        {
            switch (key)
            {
                case VirtualKey.Right or VirtualKey.Down when _favActive < favCount - 1:
                    _favActive++;
                    break;
                case VirtualKey.Left or VirtualKey.Up when _favActive > 0:
                    _favActive--;
                    break;
                case VirtualKey.Left or VirtualKey.Up:
                    _favActive = -1;
                    SyncSelection();
                    return;
                default:
                    break; // 右/下到末尾：停在原地
            }
            SyncFavSelection();
            return;
        }

        // 结果区
        if (_items.Count == 0)
        {
            if (favCount > 0 && key is VirtualKey.Down or VirtualKey.Right)
            {
                _favActive = 0;
                SyncFavSelection();
            }
            return;
        }

        if (_gridView)
        {
            var cols = Math.Max(1, GridColumns());
            switch (key)
            {
                case VirtualKey.Left:
                    if (_active > 0) _active--;
                    break;
                case VirtualKey.Right:
                    if (_active < _items.Count - 1) _active++;
                    break;
                case VirtualKey.Up:
                    if (_active >= cols) _active -= cols; // 顶行没有元素：停在原地
                    break;
                case VirtualKey.Down:
                    var below = _active + cols;
                    if (below < _items.Count)
                        _active = below;
                    else if (favCount > 0)
                    {
                        _favActive = 0;
                        SyncFavSelection();
                        return;
                    }
                    // 底行正下方没有元素（且无收藏）：停在原地，不跳到最后
                    break;
            }
        }
        else
        {
            switch (key)
            {
                case VirtualKey.Up or VirtualKey.Left:
                    if (_active > 0) _active--;
                    break;
                case VirtualKey.Down or VirtualKey.Right:
                    if (_active < _items.Count - 1)
                        _active++;
                    else if (favCount > 0)
                    {
                        _favActive = 0;
                        SyncFavSelection();
                        return;
                    }
                    break;
            }
        }
        SyncSelection();
    }

    /// <summary>收藏区选中：刷新卡片边框 + 取消结果区选中 + 聚焦当前卡片。</summary>
    private void SyncFavSelection()
    {
        UpdateFavCardStates();
        ResultList.SelectedIndex = -1;
        ResultGrid.SelectedIndex = -1;
        var i = 0;
        foreach (var child in FavItems.Children)
        {
            if (child is not Button b) continue;
            if (i == _favActive)
            {
                b.Focus(FocusState.Programmatic);
                try { b.StartBringIntoView(); } catch { /* ignore */ }
            }
            i++;
        }
    }

    /// <summary>收藏卡片按钮（排除空态占位 TextBlock）。</summary>
    private List<Button> FavButtons() => FavItems.Children.OfType<Button>().ToList();

    /// <summary>按 _favActive 刷新所有收藏卡片状态（默认无边框，光标选中才显示）。</summary>
    private void UpdateFavCardStates()
    {
        var i = 0;
        foreach (var child in FavItems.Children)
        {
            if (child is not Button b) continue;
            SetFavCardState(b, i == _favActive);
            i++;
        }
    }

    /// <summary>收藏卡片选中态：与上方平铺卡片一致——中性白底 + 白边（不用蓝色），
    /// 未选中全透明（默认无边框，光标移上去才有）。</summary>
    private void SetFavCardState(Button b, bool selected)
    {
        var res = Root.Resources;
        b.Background = selected
            ? (Brush)res["GridTileSelBgBrush"]
            : new SolidColorBrush(Colors.Transparent);
        b.BorderBrush = selected
            ? (Brush)res["GridTileSelBorderBrush"]
            : new SolidColorBrush(Colors.Transparent);
    }

    /// <summary>焦点是否落在收藏坞（或其按钮）内；是则箭头键交给原生方向键导航。</summary>
    private bool IsFocusOnFavorites()
    {
        if (FocusManager.GetFocusedElement() is not FrameworkElement fe) return false;
        for (DependencyObject? p = fe; p is not null; p = VisualTreeHelper.GetParent(p))
            if (ReferenceEquals(p, FavRoot)) return true;
        return false;
    }

    /// <summary>按已布局的第一行元素推断平铺列数（跟随窗口宽度，不用硬编码）。</summary>
    private int GridColumns()
    {
        if (ResultGrid.ItemsPanelRoot is not ItemsWrapGrid panel || panel.Children.Count == 0)
            return 1;
        var y0 = panel.Children[0].ActualOffset.Y;
        var cols = 1;
        for (var i = 1; i < panel.Children.Count; i++)
        {
            if (panel.Children[i].ActualOffset.Y > y0)
                break;
            cols++;
        }
        return cols;
    }

    private void SyncSelection()
    {
        UpdateFavCardStates();  // 结果区选中时收藏卡片保持默认无边框
        if (_gridView)
        {
            ResultGrid.SelectedIndex = _active;
            if (_active >= 0 && _active < _items.Count)
                ResultGrid.ScrollIntoView(_items[_active]);
        }
        else
        {
            ResultList.SelectedIndex = _active;
            if (_active >= 0 && _active < _items.Count)
                ResultList.ScrollIntoView(_items[_active]);
        }
    }

    private async void OnItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is not CandidateDto c) return;
        _active = _items.IndexOf(c);
        await InvokeAsync();
    }

    private async Task InvokeAsync()
    {
        if (_items.Count == 0 || _active < 0 || _active >= _items.Count) return;
        await InvokeActionAsync(_items[_active].Id, "open");
    }

    /// <summary>按 action 执行（回车/点击与右键菜单共用；含错误处理与执行后隐藏）。</summary>
    private async Task InvokeActionAsync(string itemId, string actionId, string? titleOverride = null)
    {
        var item = _items.FirstOrDefault(x => x.Id == itemId);
        var title = titleOverride ?? item?.Title ?? itemId;

        // 插件 page 命中：走 host.plugin.open 开 WebView2 窗口，不走 host.invoke。
        // target 形如 "plugin:page:<id>"（《插件开发规范》§5.4 路由契约）。
        if (item is not null && TryGetPluginPageId(item, actionId, out var pluginId))
        {
            await OpenPluginPageAsync(pluginId, item, title);
            return;
        }

        Footer.Text = "执行中：" + title;
        try
        {
            var result = await _host.InvokeAsync(itemId, actionId, QueryBox.Text ?? "");
            if (await HandleInvokeResultAsync(itemId, title, result))
            {
                return;
            }
            Footer.Text = actionId == "runas" ? "已以管理员身份打开：" + title : "已执行：" + title;
        }
        catch (Exception ex)
        {
            App.Log("Invoke", ex);
            Footer.Text = "执行失败：" + title;
            return;
        }
        // 执行后隐藏是固化默认行为（设置页不提供开关）
        await Task.Delay(60);
        HideLauncher();
    }

    private const string PluginPagePrefix = "plugin:page:";

    /// <summary>候选项（或其指定 action）是否指向插件 page，是则取出插件 id。</summary>
    private static bool TryGetPluginPageId(CandidateDto item, string actionId, out string pluginId)
    {
        var target = item.Actions.FirstOrDefault(a => a.Id == actionId)?.Target ?? item.Target;
        if (target is not null && target.StartsWith(PluginPagePrefix, StringComparison.Ordinal))
        {
            pluginId = target[PluginPagePrefix.Length..];
            return pluginId.Length > 0;
        }
        pluginId = "";
        return false;
    }

    /// <summary>
    /// 打开插件窗口。host 返回入口/窗口规格/授权，UI 侧建 WebView2 窗口。
    /// 输入上下文取自主输入框：去掉 "<keyword> " 前缀后的剩余文本。
    /// </summary>
    private async Task OpenPluginPageAsync(string pluginId, CandidateDto item, string title)
    {
        Footer.Text = "打开插件：" + title;
        var rawQuery = QueryBox.Text ?? "";
        var (command, input) = SplitPluginQuery(rawQuery);
        try
        {
            var info = await _host.PluginOpenAsync(pluginId, input, command);
            if (info is null)
            {
                Footer.Text = "插件不可用：" + title;
                return;
            }
            // 开发目录加载的插件开 DevTools，正式安装的看通用设置的开发者模式总开关（规范 §9.2）。
            // 注意：OpenOrFocus 命中旧窗口时不会重应用 dev 设置，开关切换后已开的窗口需重开才生效。
            var devMode = LocalState.Ui.DeveloperMode || await IsDevPluginAsync(pluginId);
            PluginWindowHost.OpenOrFocus(info, _host, input, command, rawQuery, devMode);
            Footer.Text = "已打开：" + title;
        }
        catch (Exception ex)
        {
            App.Log("PluginOpen", ex);
            Footer.Text = "插件打开失败：" + title;
            return;
        }
        // 执行后隐藏是固化默认行为（设置页不提供开关）
        await Task.Delay(60);
        HideLauncher();
    }

    /// <summary>
    /// 拆 "tr hello world" → ("tr", "hello world")；无空格时输入为空。
    /// 保留剩余部分的原样（不再 TrimStart），与 host 侧 find_keyword_match 的
    /// <c>trimmed[kw.len()+1..]</c> 语义一致，避免同一次触发两边算出不同的 input。
    /// </summary>
    private static (string Command, string Input) SplitPluginQuery(string rawQuery)
    {
        var trimmed = rawQuery.Trim();
        var sp = trimmed.IndexOf(' ');
        return sp < 0 ? (trimmed, "") : (trimmed[..sp], trimmed[(sp + 1)..]);
    }

    private async Task<bool> IsDevPluginAsync(string pluginId)
    {
        try
        {
            var list = await _host.PluginListAsync();
            return list.Any(p => p.Id == pluginId
                                 && string.Equals(p.Source, "dev", StringComparison.OrdinalIgnoreCase));
        }
        catch (Exception ex)
        {
            App.Log("PluginDevProbe", ex);
            return false;
        }
    }

    /// <summary>
    /// 处理 invoke 返回的特殊结果类型（show_error / confirm / copy_text / keep）。
    /// 返回 true 表示已自行处理完毕（调用方直接返回、不隐藏）；false 表示正常收尾。
    /// </summary>
    private async Task<bool> HandleInvokeResultAsync(string itemId, string title, JsonElement? result)
    {
        if (result is null || !result.Value.TryGetProperty("type", out var t))
        {
            return false;
        }
        switch (t.GetString())
        {
            case "show_error":
                if (result.Value.TryGetProperty("message", out var err))
                {
                    Footer.Text = err.GetString() ?? "执行失败";
                }
                return true;

            case "confirm":
                // 不可逆内置命令（关机/重启等）：弹确认框，确认后以 "confirm" action 重新执行
                var message = result.Value.TryGetProperty("message", out var m)
                    ? m.GetString()
                    : "确认执行？";
                if (await ConfirmDestructiveAsync(message ?? "确认执行？"))
                {
                    Footer.Text = "执行中：" + title;
                    var second = await _host.InvokeAsync(itemId, "confirm", QueryBox.Text ?? "");
                    return await HandleInvokeResultAsync(itemId, title, second);
                }
                Footer.Text = "已取消";
                return true;

            case "copy_text":
                // 信息类命令（内网IP 等）：复制到剪贴板，右下角气泡提示（学 utools）
                if (result.Value.TryGetProperty("text", out var text))
                {
                    var copied = text.GetString() ?? "";
                    var data = new DataPackage();
                    data.SetText(copied);
                    Clipboard.SetContent(data);
                    Footer.Text = "已复制：" + copied;
                    _tray?.ShowBalloon("已复制到剪贴板", $"{title}：{copied}");
                }
                return false;

            case "keep":
                // 保持打开：只显示消息（如需要继续操作的提示）
                if (result.Value.TryGetProperty("message", out var keepMsg))
                {
                    Footer.Text = keepMsg.GetString() ?? "已执行：" + title;
                }
                return true;

            default:
                return false;
        }
    }

    /// <summary>不可逆操作的确认弹窗；返回是否确认执行。</summary>
    private async Task<bool> ConfirmDestructiveAsync(string message)
    {
        if (Root.XamlRoot is null)
        {
            return false;
        }
        var dialog = new ContentDialog
        {
            Title = "确认操作",
            Content = message,
            PrimaryButtonText = "确认执行",
            CloseButtonText = "取消",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = Root.XamlRoot,
        };
        var r = await dialog.ShowAsync();
        return r == ContentDialogResult.Primary;
    }

    // ==================== P/Invoke ====================

    // ImmNotifyIME：取消当前输入法组合
    private const uint NI_COMPOSITIONSTR = 0x0010;
    private const uint CPS_CANCEL = 0x0004;

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);

    [DllImport("user32.dll")]
    private static extern IntPtr GetDC(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern int ReleaseDC(IntPtr hWnd, IntPtr hDC);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("gdi32.dll")]
    private static extern bool SetStretchBltMode(IntPtr hdc, int mode);

    [DllImport("gdi32.dll")]
    private static extern bool StretchBlt(IntPtr hdcDest, int xDest, int yDest, int wDest, int hDest,
        IntPtr hdcSrc, int xSrc, int ySrc, int wSrc, int hSrc, uint rop);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateDIBSection(IntPtr hdc, ref BITMAPINFO pbmi, uint usage,
        out IntPtr ppvBits, IntPtr hSection, uint offset);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateCompatibleDC(IntPtr hdc);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteDC(IntPtr hdc);

    [DllImport("gdi32.dll")]
    private static extern IntPtr SelectObject(IntPtr hdc, IntPtr hObject);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteObject(IntPtr hObject);

    private const int HALFTONE = 4;
    private const uint SRCCOPY = 0x00CC0020;

    [StructLayout(LayoutKind.Sequential)]
    private struct BITMAPINFO
    {
        public int biSize;              // sizeof(BITMAPINFOHEADER)=40，必须正确否则 GDI 拒绝
        public int biWidth;
        public int biHeight;
        public short biPlanes;
        public short biBitCount;
        public int biCompression;
        public int biSizeImage;
        public int biXPelsPerMeter;
        public int biYPelsPerMeter;
        public int biClrUsed;
        public int biClrImportant;
    }

    [DllImport("user32.dll")]
    private static extern int GetWindowLong(IntPtr hWnd, int nIndex);

    [DllImport("user32.dll")]
    private static extern int SetWindowLong(IntPtr hWnd, int nIndex, int dwNewLong);

    [DllImport("imm32.dll")]
    private static extern IntPtr ImmGetContext(IntPtr hWnd);

    [DllImport("imm32.dll")]
    private static extern bool ImmReleaseContext(IntPtr hWnd, IntPtr hIMC);

    [DllImport("imm32.dll")]
    private static extern bool ImmNotifyIME(IntPtr hIMC, uint dwAction, uint dwIndex, uint dwValue);

    [DllImport("imm32.dll")]
    private static extern bool ImmSetOpenStatus(IntPtr hIMC, bool fOpen);

    [DllImport("imm32.dll")]
    private static extern bool ImmGetOpenStatus(IntPtr hIMC);

    [DllImport("imm32.dll")]
    private static extern IntPtr ImmAssociateContext(IntPtr hWnd, IntPtr hIMC);

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr WindowFromPoint(POINT Point);

    /// <summary>GetAncestor uFlags：取给定窗口的顶层根窗口。</summary>
    private const uint GA_ROOT = 2;

    [DllImport("user32.dll")]
    private static extern IntPtr GetAncestor(IntPtr hWnd, uint gaFlags);

    [DllImport("user32.dll")]
    private static extern IntPtr SetActiveWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool SetCursorPos(int X, int Y);

    [DllImport("user32.dll")]
    private static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, IntPtr dwExtraInfo);

    // mouse_event 标志（原地点击，无需坐标）
    private const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    private const uint MOUSEEVENTF_LEFTUP = 0x0004;

    [DllImport("user32.dll")]
    private static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);

    // SetWindowPos 标志：隐藏态移动窗口用（不动大小/层级/焦点）
    private const uint SWP_NOSIZE = 0x0001;
    private const uint SWP_NOZORDER = 0x0004;
    private const uint SWP_NOACTIVATE = 0x0010;

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern IntPtr GetFocus();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();

    [DllImport("user32.dll")]
    private static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);

    [DllImport("user32.dll")]
    private static extern bool BringWindowToTop(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr hWnd);

    // 窗口子类（标题栏双击防最大化，见 NoMaximizeWndProc）
    private const uint WM_NCLBUTTONDBLCLK = 0x00A3;
    private const int HTCAPTION = 2;
    private const uint CaptionSubclassId = 101;

    private delegate IntPtr SUBCLASSPROC(IntPtr hWnd, uint uMsg, IntPtr wParam, IntPtr lParam,
        IntPtr uIdSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll")]
    private static extern bool SetWindowSubclass(IntPtr hWnd, SUBCLASSPROC pfnSubclass,
        UIntPtr uIdSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll")]
    private static extern bool RemoveWindowSubclass(IntPtr hWnd, SUBCLASSPROC pfnSubclass,
        UIntPtr uIdSubclass);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(IntPtr hWnd, uint uMsg, IntPtr wParam,
        IntPtr lParam, IntPtr uIdSubclass, IntPtr dwRefData);

    private delegate void WinEventDelegate(IntPtr hWinEventHook, uint eventType, IntPtr hwnd,
        int idObject, int idChild, uint dwEventThread, uint dwmsEventTime);

    [DllImport("user32.dll")]
    private static extern IntPtr SetWinEventHook(uint eventMin, uint eventMax, IntPtr hmodWinEventProc,
        WinEventDelegate lpfnWinEventProc, uint idProcess, uint idThread, uint dwFlags);

    [DllImport("user32.dll")]
    private static extern bool UnhookWinEvent(IntPtr hWinEventHook);

    private const uint EVENT_SYSTEM_FOREGROUND = 0x0003;
    private const uint WINEVENT_OUTOFCONTEXT = 0x0000;

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X, Y; }

    [StructLayout(LayoutKind.Sequential)]
    private struct MONITORINFO
    {
        public int cbSize;      // 必须初始化 = Marshal.SizeOf<MONITORINFO>()，否则 GetMonitorInfo 失败
        public RECT rcMonitor;  // 显示器完整矩形（物理像素）
        public RECT rcWork;     // 工作区（不含任务栏，物理像素）
        public uint dwFlags;
    }

    // MonitorFromPoint：找不到包含点时返回最近显示器
    private const uint MONITOR_DEFAULTTONEAREST = 0x00000002;

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINT lpPoint);

    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromPoint(POINT pt, uint dwFlags);

    [DllImport("user32.dll")]
    private static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFO lpmi);
}
