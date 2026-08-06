using System.Collections.ObjectModel;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using XamlPath = Microsoft.UI.Xaml.Shapes.Path;
using Microsoft.Win32;
using Spark.UI.Models;
using Spark.UI.Services;
using Windows.Foundation;
using Windows.Graphics;
using Windows.System;
using Windows.UI;
using WinRT.Interop;

namespace Spark.UI;

public sealed partial class MainWindow : Window
{
    private readonly ObservableCollection<CandidateDto> _items = new();
    private readonly HostIpcClient _host = new();
    private AppWindow? _appWindow;
    private TrayService? _tray;
    private ToggleWatcher? _toggleWatcher;
    private int _active;
    private bool _hideOnDeactivate = true;
    private bool _loaded;
    private bool _visible;
    private bool _gridView;
    private IntPtr _hwnd;
    private int _queryGen;
    /// <summary>前台变化钩子句柄（失焦隐藏用，不依赖 Activated 事件）。</summary>
    private IntPtr _fgHook;
    /// <summary>钩子回调委托必须持有引用，否则被 GC 回收后回调悬空导致崩溃。</summary>
    private readonly WinEventDelegate _fgHookProc;
    /// <summary>显示后短时忽略失焦，避免 ForceForeground / 热键抢焦导致闪一下就关。</summary>
    private long _ignoreDeactivateUntilTicks;
    /// <summary>合并 event + pipe 双通道重复 toggle。</summary>
    private long _lastToggleTicks;

    /// <summary>是否已把窗口放上屏幕；已显示过且没保存位置时不再重排（OS 保留拖拽后的位置）。</summary>
    private bool _everPlaced;
    /// <summary>隐藏 pop-out 动画播放中（_visible 尚未复位，此时收到 toggle 应取消关闭）。</summary>
    private bool _hideAnimating;
    /// <summary>隐藏代数：显示/取消隐藏时 +1，让已排队的 _animOut.Completed→HideNow 失配跳过（防止刚显示的窗口又被延迟的 HideNow 关掉）。</summary>
    private int _hideGen;
    /// <summary>本次隐藏动画开始时的代数。</summary>
    private int _animHideGen;
    /// <summary>IME 组词中：期间箭头键交给输入法（不拦截）。</summary>
    private bool _composing;

    // 弹出动画（对齐原型 pop-in / pop-out）
    private readonly CompositeTransform _pop = new();
    private Storyboard _animIn = new();
    private Storyboard _animOut = new();
    private bool _acrylicOk;
    /// <summary>同步设置控件时避免触发保存/换主题副作用。</summary>
    private bool _syncing;
    /// <summary>列表/平铺切换动画；_viewAnimGen 让旧动画的 Completed 回调失配（快速连续切换不误折叠面板）。</summary>
    private Storyboard? _viewAnim;
    private int _viewAnimGen;
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
    private long _favTweenStart;
    private int _favTweenMs;

    public MainWindow()
    {
        try { InitializeComponent(); }
        catch (Exception ex) { App.Log("InitializeComponent", ex); throw; }

        ExtendsContentIntoTitleBar = true;
        ResultList.ItemsSource = _items;
        ResultGrid.ItemsSource = _items;
        LocalState.Load();

        // 箭头键在根上全局接管（handledEventsToo=true：输入框有文本时会先消费箭头键并把事件标为已处理，
        // 普通 KeyDown 收不到；这里在冒泡终点仍能收到），统一只控制下面选中项
        Root.AddHandler(UIElement.KeyDownEvent, new KeyEventHandler(OnRootKeyDown), true);
        // IME 组词期间不拦截箭头（组词光标移动由输入法管），否则会破坏中文输入
        QueryBox.TextCompositionStarted += (_, _) => _composing = true;
        QueryBox.TextCompositionEnded += (_, _) => _composing = false;

        Root.RenderTransform = _pop;
        // 拖拽 caption 区域跟随窗口尺寸（宽度滑杆/布局变化）
        Root.SizeChanged += (_, _) => UpdateDragRegions();
        // 钩子回调委托持有引用防 GC（字段初始化器不能引用实例方法，放构造函数）
        _fgHookProc = OnForegroundChanged;        // 玻璃背景：先试 Acrylic，失败自动退回稳定深色（历史上有 Acrylic 闪退问题）
        try
        {
            SystemBackdrop = new DesktopAcrylicBackdrop();
            _acrylicOk = true;
        }
        catch (Exception ex) { App.Log("AcrylicBackdrop", ex); _acrylicOk = false; }

        _hideOnDeactivate = LocalState.Ui.HideOnFocusLost;
        ApplyTheme();
        SetView(LocalState.Ui.DefaultView == "grid");
        CompositionTarget.Rendering += OnFavRendering;
        BuildAnimations();

        try { SetupChrome(); } catch (Exception ex) { App.Log("SetupChrome", ex); }
        // WinUI 启动时会按 App 主题重设 DWM 深色属性，这里在 SetupChrome 之后再压一次，
        // 否则浅色系统下圆角外框会是白色（即"外面那层白色"）
        try { ApplyDwmDarkMode(); } catch (Exception ex) { App.Log("DwmDarkMode", ex); }

        _host.HostNotification += OnHostNotification;

        // 热键主路径：Host SetEvent → 这里唤醒（不依赖 pipe 推送）
        _toggleWatcher = new ToggleWatcher(() =>
            DispatcherQueue.TryEnqueue(HandleToggle));

        Activated += (_, e) =>
        {
            if (e.WindowActivationState == WindowActivationState.Deactivated)
            {
                // 刚显示时忽略失焦（抢前台过程会短暂 Deactivated）
                if (Environment.TickCount64 < _ignoreDeactivateUntilTicks)
                    return;
                if (_hideOnDeactivate && _visible && SettingsPanel.Visibility != Visibility.Visible)
                    HideLauncher();
                return;
            }
            QueryBox.Focus(FocusState.Programmatic);
        };

        Closed += async (_, _) =>
        {
            if (_fgHook != IntPtr.Zero)
            {
                try { UnhookWinEvent(_fgHook); } catch { /* ignore */ }
                _fgHook = IntPtr.Zero;
            }
            _toggleWatcher?.Dispose();
            _toggleWatcher = null;
            await _host.DisposeAsync();
            HideLauncher();  // 正常关闭按钮 = 隐藏，不是真正关闭，保留托盘
        };

        Root.Loaded += async (_, _) =>
        {
            if (_loaded) return;
            _loaded = true;
            try { await _host.EnsureConnectedAsync(); } catch (Exception ex) { App.Log("HostConnect", ex); }
            try { SetupTray(); } catch (Exception ex) { App.Log("SetupTray", ex); }
            AboutText.Text = _host.IsConnected
                ? "Spark UI · 已连接 Host · Alt+Space 唤起"
                : "Spark UI · 未连接 Host（演示）· 仍可收热键事件";
            HideLauncher();
            await RefreshResultsAsync("");
            RenderFavorites();
            ApplyFavCollapse(!LocalState.Fav.Expanded, animate: false);
            _ = MaintainHostConnectionAsync();
        };
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

        // key → ARGB；深/浅各一套，值对齐 styles.css 变量
        var pal = new Dictionary<string, uint>
        {
            ["GlassBgBrush"] = _acrylicOk ? (dark ? 0x8C1C1C1Eu : 0x8CF2F2F7u) : (dark ? 0xFF1C1C1Eu : 0xFFF2F2F7u),
            ["GlassBorderBrush"] = dark ? 0x24FFFFFFu : 0xA6FFFFFFu,
            ["TextPrimaryBrush"] = dark ? 0xFFEBEBEBu : 0xE0000000u,
            ["TextSecondaryBrush"] = dark ? 0x8CFFFFFFu : 0x80000000u,
            ["TextTertiaryBrush"] = dark ? 0x61FFFFFFu : 0x59000000u,
            ["AccentBrush"] = dark ? 0xFF0A84FFu : 0xFF007AFFu,
            ["AccentSoftBrush"] = dark ? 0x380A84FFu : 0x26007AFFu,
            ["RowHoverBrush"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["RowActiveBrush"] = dark ? 0x470A84FFu : 0x29007AFFu,
            ["RowActiveStrongBrush"] = dark ? 0x5C0A84FFu : 0x38007AFFu,
            ["DividerBrush"] = dark ? 0x1AFFFFFFu : 0x14000000u,
            ["ChipBgBrush"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["FavBgBrush"] = dark ? 0x24000000u : 0x38FFFFFFu,
            ["FooterBgBrush"] = dark ? 0x1F000000u : 0x40FFFFFFu,
            ["GridTileBgBrush"] = dark ? 0x08FFFFFFu : 0x59FFFFFFu,
            ["GridTileSelBgBrush"] = dark ? 0x1EFFFFFFu : 0x12000000u,
            ["GridTileSelBorderBrush"] = dark ? 0x40FFFFFFu : 0x33000000u,
            ["SwitchTrackOffBrush"] = dark ? 0x52303038u : 0x33000000u,
            ["StarBrush"] = 0xFFFFD60Au,
            // 列表 / 平铺 选中态
            ["ListViewItemBackgroundPointerOver"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["ListViewItemBackgroundPressed"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["ListViewItemBackgroundSelected"] = dark ? 0x470A84FFu : 0x29007AFFu,
            ["ListViewItemBackgroundSelectedPointerOver"] = dark ? 0x5C0A84FFu : 0x38007AFFu,
            ["ListViewItemBackgroundSelectedPressed"] = dark ? 0x5C0A84FFu : 0x38007AFFu,
            ["ListViewItemBorderBrushPointerOver"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["ListViewItemBorderBrushPressed"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["ListViewItemBorderBrushSelected"] = dark ? 0x470A84FFu : 0x29007AFFu,
            ["GridViewItemBackgroundPointerOver"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["GridViewItemBackgroundPressed"] = dark ? 0x14FFFFFFu : 0x0D000000u,
            ["GridViewItemBackgroundSelected"] = dark ? 0x470A84FFu : 0x29007AFFu,
            ["GridViewItemBackgroundSelectedPointerOver"] = dark ? 0x5C0A84FFu : 0x38007AFFu,
            ["GridViewItemBackgroundSelectedPressed"] = dark ? 0x5C0A84FFu : 0x38007AFFu,
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
            hl.GradientStops[0].Color = Color.FromArgb(dark ? (byte)0x14 : (byte)0x80, 0xFF, 0xFF, 0xFF);

        // 内容/系统控件主题跟随玻璃主题（浅色系统下 TextBox 光标、滚动条、弹窗默认是浅色的）
        Root.RequestedTheme = dark ? ElementTheme.Dark : ElementTheme.Light;
        ApplyDwmDarkMode();
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
        // 深色属性只管标题栏，外框要用 DWMWA_BORDER_COLOR（34，Win11 22H2+）直接指定
        const int DWMWA_BORDER_COLOR = 34;
        var border = dark ? (int)0x001E1C1Cu : (int)0x00F2F2F7u; // COLORREF 0x00BBGGRR，取玻璃底色
        DwmSetWindowAttribute(_hwnd, DWMWA_BORDER_COLOR, ref border, sizeof(int));
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
        Add(Root, "Opacity", op0, op1);
        Add(_pop, "ScaleX", sc0, sc1);
        Add(_pop, "ScaleY", sc0, sc1);
        Add(_pop, "TranslateY", ty0, ty1);
        return sb;
    }

    private void ResetPop()
    {
        Root.Opacity = 1;
        _pop.ScaleX = _pop.ScaleY = 1;
        _pop.TranslateY = 0;
    }

    // ==================== Host / 托盘 ====================

    private async Task MaintainHostConnectionAsync()
    {
        while (true)
        {
            try
            {
                if (!_host.IsConnected)
                    await _host.EnsureConnectedAsync();
            }
            catch { /* ignore */ }
            await Task.Delay(2000);
        }
    }

    private void OnHostNotification(string method)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            switch (method)
            {
                case "ui.show":
                    ShowLauncher();
                    break;
                case "ui.hide":
                    // 保护期内不关
                    if (Environment.TickCount64 < _ignoreDeactivateUntilTicks) return;
                    HideLauncher();
                    break;
                case "ui.toggle":
                    HandleToggle();
                    break;
            }
        });
    }

    /// <summary>event + pipe 可能各推一次，300ms 内只处理一次。</summary>
    private void HandleToggle()
    {
        var now = Environment.TickCount64;
        if (now - _lastToggleTicks < 300)
            return;
        _lastToggleTicks = now;

        // 兜底：_visible 与窗口真实可见性可能错位（失焦隐藏路径异常/动画中断/外部隐藏）。
        // 以真实状态为准，避免"窗口已隐藏但 _visible=true"导致快捷键第一次走关闭分支。
        if (_hwnd != IntPtr.Zero && !IsWindowVisible(_hwnd) && _visible)
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
            QueryBox.Focus(FocusState.Programmatic);
            return;
        }

        if (_visible)
            HideLauncher();
        else
            ShowLauncher();
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
            // DWM 圆角：让窗口形状本身是圆角
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
        PlaceWindow(LocalState.Ui.WindowWidth, 590);

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
            if (!_hideOnDeactivate || SettingsPanel.Visibility == Visibility.Visible) return;
            HideLauncher();
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
                _hideOnDeactivate = false;
                _tray?.Dispose();
                _tray = null;
                Close();
                Environment.Exit(0);
            }));
    }

    private void PlaceWindow(int w, int h)
    {
        if (_appWindow is null) return;
        _appWindow.Resize(new SizeInt32(w, h));
        _appWindow.Title = "Spark";
        var area = DisplayArea.GetFromWindowId(_appWindow.Id, DisplayAreaFallback.Primary);
        if (area is null) return;
        var work = area.WorkArea;

        var hasSaved = LocalState.Ui.WindowX >= 0 && LocalState.Ui.WindowY >= 0;
        var saved = hasSaved ? SavedWindowPos(w, h) : null;
        if (saved is PointInt32 p)
        {
            _appWindow.Move(p);
        }
        else if (!_everPlaced || hasSaved)
        {
            // 首次显示，或保存的位置已失效（显示器拔掉等）→ 居中
            _appWindow.Move(new PointInt32(
                work.X + (work.Width - w) / 2,
                work.Y + Math.Max(80, work.Height / 6)));
        }
        // 其余情况不重排：窗口已显示过，OS 保留拖拽后的位置
        _everPlaced = true;
    }

    /// <summary>已保存的窗口位置；其中心点不再落在任何显示器工作区内时视为失效，返回 null。</summary>
    private PointInt32? SavedWindowPos(int w, int h)
    {
        var x = LocalState.Ui.WindowX;
        var y = LocalState.Ui.WindowY;
        if (x < 0 || y < 0) return null;
        var cx = x + w / 2;
        var cy = y + h / 2;
        // 不用 DisplayArea.FindAll()：该 API 在部分 WinAppSDK 版本上枚举会抛 InvalidCastException。
        // 这里用 Win32 枚举显示器（虚拟屏幕坐标，与窗口位置同坐标系）。
        var onScreen = false;
        EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero,
            (IntPtr hMon, IntPtr hdc, ref RECT r, IntPtr data) =>
            {
                if (cx >= r.Left && cx < r.Right && cy >= r.Top && cy < r.Bottom)
                    onScreen = true;
                return true;
            }, IntPtr.Zero);
        return onScreen ? new PointInt32(x, y) : null;
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

    // ==================== 显示 / 隐藏 ====================

    public void ShowLauncher()
    {
        try
        {
            // 先标记可见 + 保护期，再 Show，避免中间 Deactivated 立刻 Hide
            _visible = true;
            _ignoreDeactivateUntilTicks = Environment.TickCount64 + 500;

            if (_hwnd == IntPtr.Zero)
                _hwnd = WindowNative.GetWindowHandle(this);

            try { _appWindow?.Show(true); } catch { /* ignore */ }
            ShowWindow(_hwnd, 9);  // SW_RESTORE
            ShowWindow(_hwnd, 5);  // SW_SHOW

            // Show 之后 Resize 才生效（窗口未显示时 Resize 会被忽略）
            PlaceWindow(LocalState.Ui.WindowWidth, 590);
            // 防止 WinUI 在显示时把 DLGFRAME 样式加回来（白圈来源）
            try { MakeFrameless(); } catch { /* ignore */ }

            ForceForeground();
            Activate();
            SettingsPanel.Visibility = Visibility.Collapsed;
            QueryBox.Text = "";
            _ = RefreshResultsAsync("");
            QueryBox.Focus(FocusState.Programmatic);
            // 前台锁偶发拦截 SetForegroundWindow（窗口显示但未激活 → 点击外面不会触发失焦隐藏）。
            // 延迟重试几次，确保窗口真正拿到前台。
            _ = RetryFocusAsync();

            // pop-in（对齐原型 .launcher 入场）
            _hideGen++;  // 使已排队的隐藏动画 Completed→HideNow 失配，防止刚显示的窗口被延迟关掉
            _hideAnimating = false;
            _animOut.Stop();
            if (LocalState.Ui.ReduceMotion)
            {
                ResetPop();
            }
            else
            {
                Root.Opacity = 0;
                _pop.ScaleX = _pop.ScaleY = 0.96;
                _pop.TranslateY = 6;
                _animIn.Begin();
            }

            // 焦点落稳后再延长一点保护
            _ignoreDeactivateUntilTicks = Environment.TickCount64 + 400;
        }
        catch (Exception ex)
        {
            App.Log("ShowLauncher", ex);
        }
    }

    /// <summary>显示后延迟重试抢前台，直到窗口确实处于前台或状态变化。</summary>
    private async Task RetryFocusAsync()
    {
        for (var i = 0; i < 3; i++)
        {
            await Task.Delay(120);
            if (!_visible || _hwnd == IntPtr.Zero)
                return;
            if (GetForegroundWindow() == _hwnd)
                return;
            ForceForeground();
        }
    }

    public void HideLauncher()
    {
        // 保护期内禁止隐藏（防闪关）
        if (Environment.TickCount64 < _ignoreDeactivateUntilTicks && _visible)
            return;
        if (!_visible)
            return;

        // pop-out（对齐原型 .launcher.closing），完成后才真正隐藏
        if (LocalState.Ui.ReduceMotion)
        {
            HideNow();
            return;
        }
        _hideAnimating = true;
        _animHideGen = ++_hideGen;
        _animIn.Stop();
        _animOut.Begin();
    }
    private void HideNow()
    {
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

            // AttachThreadInput 技巧，提高 SetForegroundWindow 成功率
            var fg = GetForegroundWindow();
            var fgTid = GetWindowThreadProcessId(fg, out _);
            var curTid = GetCurrentThreadId();
            if (fgTid != curTid)
                AttachThreadInput(fgTid, curTid, true);

            SetForegroundWindow(_hwnd);
            BringWindowToTop(_hwnd);
            SetWindowPos(_hwnd, new IntPtr(-1), 0, 0, 0, 0, 0x0001 | 0x0002); // HWND_TOPMOST
            SetWindowPos(_hwnd, new IntPtr(-2), 0, 0, 0, 0, 0x0001 | 0x0002); // HWND_NOTOPMOST
            // 保持 AlwaysOnTop 由 OverlappedPresenter 管；这里只是抢焦点

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

        _items.Clear();
        var i = 0;
        foreach (var item in result.Items)
        {
            item.Shortcut = i < 9 ? $"{i + 1}" : "";
            var hint = item.Target ?? item.IconPath;
            var id = item.Id;
            // GetIcon 内部会创建 BitmapImage（必须 UI 线程），且演示数据量小，直接在 UI 线程跑
            item.IconImage = AppIconService.GetIcon(id, hint);
            if (gen != _queryGen) return;
            _items.Add(item);
            i++;
        }

        var hostTag = _host.IsConnected ? "Host · 极速" : "演示 · 本地";
        SearchMeta.Text = _items.Count > 0 ? $"{_items.Count} 项" : "";
        Footer.Text = _items.Count > 0 ? hostTag : "未找到相关结果";
        // 搜索时收藏区变淡（对齐原型 dimmed）
        FavRoot.Opacity = string.IsNullOrWhiteSpace(q) ? 1.0 : 0.45;

        if (_items.Count > 0)
        {
            _active = 0;
            ResultList.SelectedIndex = 0;
            ResultGrid.SelectedIndex = 0;
        }
    }

    // ==================== 收藏坞 ====================

    private void RenderFavorites()
    {
        FavGroups.Children.Clear();
        FavItems.Children.Clear();
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
                FontWeight = active ? FontWeights.SemiBold : FontWeights.Medium,
                Padding = new Thickness(10, 4, 10, 4),
                CornerRadius = new CornerRadius(999),
                BorderThickness = new Thickness(1),
                Background = active ? (Brush)res["AccentSoftBrush"] : new SolidColorBrush(Colors.Transparent),
                BorderBrush = active ? (Brush)res["RowActiveBrush"] : new SolidColorBrush(Colors.Transparent),
                Foreground = active ? (Brush)res["TextPrimaryBrush"] : (Brush)res["TextTertiaryBrush"],
            };
            var gid = g.Id;
            btn.Click += (_, _) =>
            {
                if (LocalState.Fav.ActiveGroup != gid)
                {
                    LocalState.Fav.ActiveGroup = gid;
                    LocalState.SaveFav();
                    RenderFavorites();
                }
            };
            FavGroups.Children.Add(btn);
        }

        // 收藏项（按分组过滤；无收藏时给默认演示项）
        List<string> ids;
        if (fav.Items.Count == 0)
            ids = new List<string> { "app.wt", "app.code", "app.chrome", "app.explorer", "sys.settings" };
        else
            ids = fav.Items
                .Where(x => fav.ActiveGroup == "all" || x.GroupId == fav.ActiveGroup)
                .Select(x => x.ItemId).Distinct().ToList();

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
            var c = _items.FirstOrDefault(x => x.Id == id) ?? DemoData.Find(id);
            if (c is null) continue;

            var imgSrc = AppIconService.GetIcon(id, c.Target ?? c.IconPath);
            UIElement iconEl;
            if (imgSrc is not null)
            {
                iconEl = new Image
                {
                    Source = imgSrc, Width = 36, Height = 36, Stretch = Stretch.Uniform,
                    HorizontalAlignment = HorizontalAlignment.Center,
                    VerticalAlignment = VerticalAlignment.Center
                };
            }
            else
            {
                iconEl = new Border
                {
                    Width = 36, Height = 36, CornerRadius = new CornerRadius(10),
                    Background = c.IconBrush,
                    Child = new TextBlock
                    {
                        Text = c.IconGlyph, FontSize = 12, FontWeight = FontWeights.SemiBold,
                        Foreground = new SolidColorBrush(Colors.White),
                        HorizontalAlignment = HorizontalAlignment.Center,
                        VerticalAlignment = VerticalAlignment.Center
                    }
                };
            }

            var panel = new StackPanel
            {
                Width = 72, Spacing = 6, Padding = new Thickness(6, 8, 6, 8)
            };
            panel.Children.Add(iconEl);
            panel.Children.Add(new TextBlock
            {
                Text = c.Title, FontSize = 10, Foreground = (Brush)res["TextSecondaryBrush"],
                TextAlignment = TextAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis,
                MaxLines = 1
            });

            var btn = new Button
            {
                Content = panel,
                Padding = new Thickness(0),
                Background = new SolidColorBrush(Color.FromArgb(0x0A, 0xFF, 0xFF, 0xFF)),
                BorderThickness = new Thickness(1),
                BorderBrush = (Brush)res["DividerBrush"],
                CornerRadius = new CornerRadius(12),
                Tag = c.Id
            };
            ToolTipService.SetToolTip(btn, c.Title);
            var itemId = c.Id;
            var title = c.Title;
            btn.Click += async (_, _) =>
            {
                try
                {
                    await _host.InvokeAsync(itemId, "open", QueryBox.Text ?? "");
                    Footer.Text = "已执行：" + title;
                }
                catch (Exception ex) { App.Log("FavInvoke", ex); Footer.Text = "执行失败：" + title; }
                if (LocalState.Ui.HideAfterInvoke) HideLauncher();
            };
            FavItems.Children.Add(btn);
        }

        // 折叠状态由 ApplyFavCollapse 管（带动画），这里只同步提示文字
        ToolTipService.SetToolTip(FavToggle, fav.Expanded ? "收起收藏" : "展开收藏");
    }

    /// <summary>
    /// 收藏坞折叠态：像抽屉一样推回/拉出——主体高度逐帧伸缩 + 淡入淡出 + 箭头旋转 + 分组行淡出
    /// （对齐原型 .favorites.is-collapsed 过渡）。animate=false 时直接落定（启动、新建分组、减少动画）。
    /// </summary>
    private void ApplyFavCollapse(bool collapsed, bool animate)
    {
        _favTweening = false;
        _favAnimGen++;
        var gen = _favAnimGen;

        if (!animate || LocalState.Ui.ReduceMotion)
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
            FavBody.Visibility = Visibility.Visible;
            FavBody.Height = double.NaN;
            FavBody.UpdateLayout();
            h = FavBody.ActualHeight > 0 ? FavBody.ActualHeight : 76;
            _favGroupsW0 = FavGroupsCol.ActualWidth > 0 ? FavGroupsCol.ActualWidth : 300;
            // 量完立即压回 0 并应用布局：否则 Timer 首帧前会按自然高度闪出一帧完整抽屉
            // （展开时"弹一下"的来源：完整展开一帧 → 瞬间缩回 0 → 再平滑拉出）
            FavBody.Height = 0;
            FavBody.UpdateLayout();
        }

        // 起点取当前实际值，连续快速切换可从中途续动
        _favCollapsing = collapsed;
        _favH0 = collapsed ? h : 0;
        _favH1 = collapsed ? 0 : h;
        _favO0 = FavBody.Opacity;
        _favG0 = FavGroups.Opacity;
        _favA0 = FavChevronRotate.Angle;
        _favS0 = FavChevronShift.Y;
        _favTweenStart = Environment.TickCount64;
        // 展开 240ms 轻快，收起 280ms（1.5 次方曲线下前后更均匀）
        _favTweenMs = collapsed ? 280 : 240;
        FavBody.Visibility = Visibility.Visible;
        _favTweening = true;
    }

    /// <summary>每渲染帧：按缓动曲线插值高度/透明度/箭头/分组行，结束后落定。</summary>
    private void OnFavRendering(object? sender, object e)
    {
        if (!_favTweening) return;
        var t = (Environment.TickCount64 - _favTweenStart) / (double)_favTweenMs;
        var done = t >= 1;
        if (done) t = 1;
        // 缓动：收起走 1-(1-t)^1.5——速度从 1.5 线性降到 0，比 quadratic（2→0）前后段更均匀、
        // 不拖尾（quadratic 后段明显偏慢，感知"前后不和谐"）；展开保持 quadratic（轻快）
        var k = _favCollapsing
            ? 1 - Math.Pow(1 - t, 1.5)
            : 1 - Math.Pow(1 - t, 2);

        FavBody.Height = _favH0 + (_favH1 - _favH0) * k;
        FavBody.Opacity = _favO0 + ((_favCollapsing ? 0 : 1) - _favO0) * k;
        FavChevronRotate.Angle = _favA0 + ((_favCollapsing ? -90 : 0) - _favA0) * k;
        // 收起后箭头旋转成竖长 ">"：视觉重心比星星/文字高约 2px，随动画下移对齐（展开恢复）
        FavChevronShift.Y = _favS0 + ((_favCollapsing ? 2 : 0) - _favS0) * k;
        FavGroups.Opacity = _favG0 + ((_favCollapsing ? 0 : 1) - _favG0) * k;
        FavAddGroup.Opacity = _favG0 + ((_favCollapsing ? 0 : 1) - _favG0) * k;
        // 布局型属性（间距/分组列宽/按钮宽）只在前半程收放，后半程保持终值——
        // 避免尾部多布局属性叠加（ScrollViewer 裁剪 + 列宽重排）导致单帧超时、动画"直接跳到底"
        var p = _favCollapsing ? 1 - Math.Min(1, (1 - k) * 2) : Math.Min(1, k * 2);
        FavRoot.Spacing = 8 * p;
        FavGroupsCol.Width = new GridLength(_favGroupsW0 * p);
        FavAddGroup.Width = 22 * p;

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
        FavBody.Visibility = collapsed ? Visibility.Collapsed : Visibility.Visible;
        FavBody.Height = double.NaN;
        FavBody.Opacity = 1;
        FavRoot.Spacing = collapsed ? 0 : 8;
        FavGroupsCol.Width = collapsed ? new GridLength(0) : new GridLength(1, GridUnitType.Star);
        FavAddGroup.Width = collapsed ? 0 : 22;
        FavGroups.Opacity = collapsed ? 0 : 1;
        FavAddGroup.Opacity = collapsed ? 0 : 1;
        FavGroups.IsHitTestVisible = FavAddGroup.IsHitTestVisible = !collapsed;
        FavChevronRotate.Angle = collapsed ? -90 : 0;
        FavChevronShift.Y = collapsed ? 2 : 0;
    }

    private void OnFavToggle(object sender, RoutedEventArgs e)
    {
        var fav = LocalState.Fav;
        fav.Expanded = !fav.Expanded;
        LocalState.SaveFav();
        RenderFavorites();
        ApplyFavCollapse(!fav.Expanded, animate: true);
    }

    private async void OnFavAddGroup(object sender, RoutedEventArgs e)
    {
        var box = new TextBox { PlaceholderText = "新分组名称", MaxLength = 8 };
        var dlg = new ContentDialog
        {
            Title = "新建分组",
            Content = box,
            PrimaryButtonText = "创建",
            CloseButtonText = "取消",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Root.XamlRoot,
        };
        if (await dlg.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrWhiteSpace(box.Text))
        {
            var name = box.Text.Trim();
            if (name.Length > 8) name = name[..8];
            var id = "g_" + Convert.ToString(DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(), 36);
            LocalState.Fav.Groups.Add(new FavGroupDto { Id = id, Name = name });
            LocalState.Fav.ActiveGroup = id;
            LocalState.Fav.Expanded = true;
            LocalState.SaveFav();
            RenderFavorites();
            ApplyFavCollapse(false, animate: false);
        }
    }

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

        if (LocalState.Ui.ReduceMotion)
        {
            ResultList.Opacity = 1;
            ResultGrid.Opacity = 1;
            outgoing.Visibility = Visibility.Collapsed;
            incoming.Visibility = Visibility.Visible;
            return;
        }

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

    private void OnOpenSettings(object sender, RoutedEventArgs e)
    {
        SettingsPanel.Visibility = Visibility.Visible;
        AboutText.Text = _host.IsConnected
            ? "Spark UI · 已连接 Host"
            : "Spark UI · 未连接 Host（演示）";
        SyncSettingsUi();
        ShowPane("general");
        UpdateDragRegions();
    }

    private void OnCloseSettings(object sender, RoutedEventArgs e)
    {
        SettingsPanel.Visibility = Visibility.Collapsed;
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
        => ShowPane((string)((Button)sender).Tag);

    private void ShowPane(string pane)
    {
        PaneGeneral.Visibility = pane == "general" ? Visibility.Visible : Visibility.Collapsed;
        PaneHotkey.Visibility = pane == "hotkey" ? Visibility.Visible : Visibility.Collapsed;
        PaneAppearance.Visibility = pane == "appearance" ? Visibility.Visible : Visibility.Collapsed;
        PanePlugins.Visibility = pane == "plugins" ? Visibility.Visible : Visibility.Collapsed;

        var res = Root.Resources;
        foreach (var b in new[] { NavGeneral, NavHotkey, NavAppearance, NavPlugins })
        {
            var on = (string)b.Tag == pane;
            b.Background = on ? (Brush)res["AccentSoftBrush"] : new SolidColorBrush(Colors.Transparent);
            b.Foreground = on ? (Brush)res["TextPrimaryBrush"] : (Brush)res["TextSecondaryBrush"];
            b.FontWeight = on ? FontWeights.SemiBold : FontWeights.Normal;
        }
    }

    /// <summary>打开设置时把 LocalState 同步到控件（期间不触发保存副作用）。</summary>
    private void SyncSettingsUi()
    {
        _syncing = true;
        try
        {
            StartupSwitch.IsOn = LocalState.Ui.LaunchOnStartup;
            HideFocusSwitch.IsOn = LocalState.Ui.HideOnFocusLost;
            HideInvokeSwitch.IsOn = LocalState.Ui.HideAfterInvoke;
            ThemeCombo.SelectedIndex = LocalState.Ui.Theme switch { "light" => 2, "dark" => 1, _ => 0 };
            ViewCombo.SelectedIndex = LocalState.Ui.DefaultView == "grid" ? 1 : 0;
            WidthSlider.Value = LocalState.Ui.WindowWidth;
            WidthValue.Text = $"{LocalState.Ui.WindowWidth}px";
            ReduceMotionSwitch.IsOn = LocalState.Ui.ReduceMotion;
            UpdateHotkeyPresets();
        }
        finally { _syncing = false; }
    }

    private void OnToggleStartup(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        LocalState.Ui.LaunchOnStartup = StartupSwitch.IsOn;
        LocalState.SaveUi();
    }

    private void OnToggleHideFocus(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        LocalState.Ui.HideOnFocusLost = HideFocusSwitch.IsOn;
        _hideOnDeactivate = HideFocusSwitch.IsOn;
        LocalState.SaveUi();
    }

    private void OnToggleHideInvoke(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        LocalState.Ui.HideAfterInvoke = HideInvokeSwitch.IsOn;
        LocalState.SaveUi();
    }

    private void OnToggleReduceMotion(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        LocalState.Ui.ReduceMotion = ReduceMotionSwitch.IsOn;
        LocalState.SaveUi();
        _hideGen++;
        _hideAnimating = false;
        _animIn.Stop();
        _animOut.Stop();
        ResetPop();
        // 停掉列表/平铺切换动画，按当前视图直接落定（防止中断在半透明/半缩放状态）
        _viewAnim?.Stop();
        _viewAnimGen++;
        ResultList.Visibility = _gridView ? Visibility.Collapsed : Visibility.Visible;
        ResultGrid.Visibility = _gridView ? Visibility.Visible : Visibility.Collapsed;
        ResultList.Opacity = 1;
        ResultGrid.Opacity = 1;
        // 收藏坞同理直接落定
        ApplyFavCollapse(!LocalState.Fav.Expanded, animate: false);
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

    private void OnWidthChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        var w = (int)Math.Round(e.NewValue);
        WidthValue.Text = $"{w}px";
        if (_syncing) return;
        // Slider 初始化（Value 从 0 钳到 Minimum）也会触发本事件；
        // 只在设置页可见时才真正改窗口宽度，避免启动时误保存
        if (SettingsPanel.Visibility != Visibility.Visible) return;
        LocalState.Ui.WindowWidth = w;
        LocalState.SaveUi();
        if (_appWindow is not null)
            PlaceWindow(w, 590);
    }

    private void OnHotkeyPreset(object sender, RoutedEventArgs e)
    {
        LocalState.Ui.Hotkey = (string)((Button)sender).Tag;
        LocalState.SaveUi();
        UpdateHotkeyPresets();
    }

    private void UpdateHotkeyPresets()
    {
        var res = Root.Resources;
        foreach (var b in new[] { BtnHotkeyAlt, BtnHotkeyCtrl })
        {
            var on = (string)b.Tag == LocalState.Ui.Hotkey;
            b.Background = on ? (Brush)res["AccentSoftBrush"] : (Brush)res["ChipBgBrush"];
            b.BorderBrush = on ? (Brush)res["AccentBrush"] : (Brush)res["GlassBorderBrush"];
            b.Foreground = on ? (Brush)res["TextPrimaryBrush"] : (Brush)res["TextSecondaryBrush"];
        }
    }

    private void OnInstallPlugin(object sender, RoutedEventArgs e)
        => PluginStatus.Text = "安装本地插件：暂未实现（原型占位）。";

    // ==================== 键盘 / 执行 ====================

    private async void OnQueryChanged(object sender, TextChangedEventArgs e)
        => await RefreshResultsAsync(QueryBox.Text ?? "");

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

        if (e.Key == VirtualKey.Escape)
        {
            e.Handled = true;
            if (SettingsPanel.Visibility == Visibility.Visible)
            {
                OnCloseSettings(sender, e);
                return;
            }
            if (!string.IsNullOrEmpty(QueryBox.Text))
            {
                QueryBox.Text = "";
                await RefreshResultsAsync("");
            }
            else HideLauncher();
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
    /// 这里仍能收到，统一只移动下面选中项，并把输入框光标拉回末尾（搜索框只能输入/退格，不能移光标）。
    /// 列表/平铺都走这里：列表视图左右等价上一条/下一条，平铺按网格行列移动。
    /// 焦点在收藏坞交互件上时不接管（方向键留给按钮导航）；其余位置（搜索框/列表/平铺/视图切换…）箭头键都移动结果选中项——
    /// 列表视图点击条目后焦点落在 ListViewItem 上，原生 ListView 只处理上下、左右是死键（平铺原生是 2D 没这问题），
    /// 必须在这里统一接管，两视图行为才一致。
    /// 注意：焦点在条目上时原生 ListView/GridView 会先于本处理器移动选中（键盘光标跟随），
    /// 因此这里从 _active（按下前的值）计算目标并绝对赋值；若读 SelectedIndex 重算会叠加上原生那一步，造成双重移动。</summary>
    private void OnRootKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (_composing) return;                       // IME 组词中不拦截
        if (MainPanel.Visibility != Visibility.Visible) return; // 设置页里不动
        if (IsFocusOnFavorites()) return;             // 焦点在收藏坞：交给原生方向键导航
        if (e.Key is not (VirtualKey.Down or VirtualKey.Up or VirtualKey.Left or VirtualKey.Right)) return;

        e.Handled = true;
        QueryBox.SelectionStart = QueryBox.Text.Length;
        QueryBox.SelectionLength = 0;

        if (_items.Count == 0) return;
        int delta;
        if (_gridView)
        {
            // 平铺视图：按网格行列移动（列数跟随窗口宽度）
            var cols = GridColumns();
            delta = e.Key switch
            {
                VirtualKey.Left => -1,
                VirtualKey.Right => 1,
                VirtualKey.Up => -cols,
                _ => cols,
            };
        }
        else
        {
            // 列表视图：左右等价于上一条/下一条
            delta = e.Key is VirtualKey.Up or VirtualKey.Left ? -1 : 1;
        }
        _active = Math.Clamp(_active + delta, 0, _items.Count - 1);
        SyncSelection();
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
        var item = _items[_active];
        Footer.Text = "执行中：" + item.Title;
        try
        {
            var result = await _host.InvokeAsync(item.Id, "open", QueryBox.Text ?? "");
            if (result is not null && result.Value.TryGetProperty("type", out var t)
                && t.GetString() == "show_error"
                && result.Value.TryGetProperty("message", out var msg))
            {
                Footer.Text = msg.GetString() ?? "执行失败";
                return;
            }
            Footer.Text = "已执行：" + item.Title;
        }
        catch (Exception ex)
        {
            App.Log("Invoke", ex);
            Footer.Text = "执行失败：" + item.Title;
            return;
        }
        if (LocalState.Ui.HideAfterInvoke)
        {
            await Task.Delay(60);
            HideLauncher();
        }
    }

    // ==================== P/Invoke ====================

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);

    [DllImport("user32.dll")]
    private static extern int GetWindowLong(IntPtr hWnd, int nIndex);

    [DllImport("user32.dll")]
    private static extern int SetWindowLong(IntPtr hWnd, int nIndex, int dwNewLong);

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

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

    private delegate bool EnumMonitorsProc(IntPtr hMonitor, IntPtr hdcMonitor, ref RECT lprcMonitor, IntPtr dwData);

    [DllImport("user32.dll")]
    private static extern bool EnumDisplayMonitors(IntPtr hdc, IntPtr lprcClip, EnumMonitorsProc lpfnEnum, IntPtr dwData);
}
