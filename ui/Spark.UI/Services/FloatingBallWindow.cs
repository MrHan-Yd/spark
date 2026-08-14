using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Foundation;
using Windows.Graphics;
using Windows.UI;
using WinRT.Interop;

namespace Spark.UI.Services;

/// <summary>
/// 桌面悬浮球（参考 uTools 悬浮球）：贴边驻留 + 自由悬浮双模式、悬停滑出 + 拖拽换边/换位、
/// 点击唤起主窗口。独立置顶小窗（ToolWindow，不进任务栏/Alt+Tab），生命周期由
/// 「通用设置 → 悬浮球」开关控制，不随主窗口隐藏/关闭。
/// 位置状态（贴边方向/自由悬浮坐标 + 模式）持久化在 LocalState.Ui。
///
/// 交互约定：
/// - 拖到屏幕边缘（工作区边缘 ≤ DockThresholdDip）→ 贴边驻留：平时只露出 SliverDip 宽的
///   窄条（悬停命中区 = 窗口边界，天然支持），鼠标移入滑出全圆、移出滑回收起；
/// - 拖到桌面其他地方 → 自由悬浮：保持落点常显全圆，悬停不做收起动画；
/// - 左键按住拖动（位移 &gt; DragThresholdDip 判定为拖，否则为点）；松手按上述规则落位；
/// - 拖拽被系统打断（手掌误触/捕获被夺走）→ 按松手规则落位，不触发点击；
/// - 左键点击（无拖动）= 唤起/隐藏主窗口（由 MainWindow 回调决定）；
/// - 右键 = 菜单（打开主界面 / 退出 Spark）。
/// </summary>
public sealed class FloatingBallWindow : Window
{
    private const double BallSizeDip = 46;       // 悬浮球直径（DIP）
    private const double SliverDip = 14;         // 贴边时露出的宽度（DIP）
    private const double DragThresholdDip = 6;   // 位移超过才算拖拽（DIP）
    private const double DockThresholdDip = 16;  // 松手时离工作区边缘小于等于此值 → 贴边驻留（DIP）
    private const int SlideMs = 140;             // 滑出/收起动画时长
    private const int SlideIntervalMs = 16;      // 动画帧间隔（位置动画用定时器驱动）

    private readonly Action _onToggle;
    private readonly Action _onShow;
    private readonly Action _onExit;
    private readonly Border _ball = new();
    private readonly DispatcherQueueTimer _slideTimer;
    private AppWindow? _appWindow;
    private IntPtr _hwnd;

    private double _scale = 1.0;
    private int _w, _h;                          // 窗口物理尺寸（正方形）
    private string _side = "right";              // 贴边方向（left/right/top/bottom）
    private int _x, _y;                          // 当前窗口位置（物理像素，屏幕坐标）
    /// <summary>驻留模式：true = 贴边驻留（收起/滑出），false = 自由悬浮（常显全圆，位置任意）。
    /// 由 EndDrag（松手落位）、PlaceInitial、OnXamlRootChanged 共同维护。</summary>
    private bool _docked;
    /// <summary>当前贴边状态：false = 收起（只露 sliver），true = 全显。
    /// 仅贴边模式有意义（自由悬浮恒为 true）。由 Entered/Exited/按下/吸附/缩放重贴边共同维护。</summary>
    private bool _expanded;

    // 指针状态
    private bool _captured;
    private bool _dragging;
    private Point _pressDip;                     // 按下位置（DIP）
    private Point _lastDip;                      // 上次指针位置（DIP），拖拽增量用

    // 滑出/收起动画（只动贴边方向上的坐标轴：左右贴边只动 X，上下贴边只动 Y）
    private int _slideFromX, _slideFromY, _slideToX, _slideToY;
    private long _slideStart;
    /// <summary>当前滑动的目标方向（true = 滑出全显，false = 滑回收起）；动画完成时落 _expanded。</summary>
    private bool _slideExpanding;

    // 深/浅主题下的圆底与描边（浅色桌面下深色球也清晰，颜色随主题切换；
    // 圆形图标 ball.png 覆盖整个球面后圆底仅作兜底，描边仍随主题呈现）
    private static readonly SolidColorBrush DarkBg =
        new(Color.FromArgb(0xE6, 0x1C, 0x1C, 0x1E));
    private static readonly SolidColorBrush DarkBorder =
        new(Color.FromArgb(0x59, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush LightBg =
        new(Color.FromArgb(0xE6, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush LightBorder =
        new(Color.FromArgb(0x33, 0x00, 0x00, 0x00));

    public FloatingBallWindow(bool dark, Action onToggle, Action onShow, Action onExit)
    {
        _onToggle = onToggle;
        _onShow = onShow;
        _onExit = onExit;

        // 与主窗口同款：构造即显示，先藏起来配置好再 Show，避免裸窗闪一下
        this.AppWindow.Hide();

        _slideTimer = DispatcherQueue.CreateTimer();
        _slideTimer.Interval = TimeSpan.FromMilliseconds(SlideIntervalMs);
        _slideTimer.Tick += (_, _) => SlideTick();

        _ball.Width = _ball.Height = BallSizeDip;
        _ball.CornerRadius = new CornerRadius(BallSizeDip / 2);
        _ball.BorderThickness = new Thickness(1);
        _ball.Child = CreateIcon();
        ApplyTheme(dark);
        AutomationProperties.SetName(_ball, "Spark 悬浮球：点击唤起或隐藏主窗口");
        _ball.PointerPressed += OnBallPressed;
        _ball.PointerMoved += OnBallMoved;
        _ball.PointerReleased += OnBallReleased;
        _ball.PointerCanceled += OnBallCanceled;
        _ball.PointerCaptureLost += OnBallCaptureLost;
        _ball.PointerEntered += OnBallEntered;
        _ball.PointerExited += OnBallExited;
        _ball.RightTapped += OnBallRightTapped;
        Content = _ball;

        SetupChrome();
        PlaceInitial();

        // 内容挂载后 XamlRoot 才有真实缩放：与目标屏估算不一致（跨 DPI 屏/兜底路径）时
        // 按真实值重摆（knownScale 传入，不再被光标屏 DPI 覆盖）；之后监听缩放变化
        // （跨屏拖拽/系统缩放调整），窗口尺寸与贴边位置实时跟随
        _ball.Loaded += (_, _) =>
        {
            var scale = _ball.XamlRoot?.RasterizationScale ?? 0;
            if (scale > 0 && Math.Abs(scale - _scale) > 0.02)
                PlaceInitial(scale);
            if (_ball.XamlRoot is { } xr)
                xr.Changed += OnXamlRootChanged;
        };

        try { _appWindow?.Show(false); }  // 不抢焦点：开机自启/隐藏模式下悬浮球默默驻留
        catch (Exception ex) { App.Log("Ball", ex); }
    }

    // ==================== 窗口骨架 ====================

    /// <summary>置顶无边框工具窗：无标题栏、不进任务栏/Alt+Tab，DWM 不裁圆角
    /// （圆角会把圆形切出缺口，透明四角由内容层（圆底）自行呈现）。</summary>
    private void SetupChrome()
    {
        _hwnd = WindowNative.GetWindowHandle(this);
        _appWindow = AppWindow.GetFromWindowId(Win32Interop.GetWindowIdFromWindow(_hwnd));
        _appWindow.Title = "Spark";
        if (_appWindow.Presenter is OverlappedPresenter p)
        {
            p.IsResizable = false;
            p.IsMaximizable = false;
            p.IsMinimizable = false;
            p.SetBorderAndTitleBar(false, false);
            p.IsAlwaysOnTop = true;
        }
        try { _appWindow.IsShownInSwitchers = false; } catch { /* older SDK */ }

        const int GWL_EXSTYLE = -20;
        const int WS_EX_TOOLWINDOW = 0x00000080;
        const int WS_EX_APPWINDOW = 0x00040000;
        var exStyle = GetWindowLong(_hwnd, GWL_EXSTYLE);
        exStyle = (exStyle | WS_EX_TOOLWINDOW) & ~WS_EX_APPWINDOW;
        SetWindowLong(_hwnd, GWL_EXSTYLE, exStyle);
        // FRAMECHANGED 让扩展样式立即生效（无边框窗口也按样式重算非客户区）
        SetWindowPos(_hwnd, IntPtr.Zero, 0, 0, 0, 0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);

        const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
        const int DWMWCP_DONOTROUND = 1;
        var pref = DWMWCP_DONOTROUND;
        DwmSetWindowAttribute(_hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref pref, sizeof(int));

        const int DWMWA_ALLOW_CLIENT_AREA_TO_FILL_ENTIRE_WINDOW = 14;
        var allow = 1;
        DwmSetWindowAttribute(_hwnd, DWMWA_ALLOW_CLIENT_AREA_TO_FILL_ENTIRE_WINDOW, ref allow, sizeof(int));

        // 显示器拓扑变化自愈（拔屏后球悬空在消失的屏幕上，鼠标够不到）
        try
        {
            _subclassGc = GCHandle.Alloc(this);
            // 安装失败（返回 false）：没有回调引用 dwRefData，立即释放句柄表槽位，
            // 否则随开关反复重建累积泄漏（Free 后 IsAllocated 自动为 false）
            if (!SetWindowSubclass(_hwnd, _wndProc, new UIntPtr(SubclassId), GCHandle.ToIntPtr(_subclassGc)))
                _subclassGc.Free();
        }
        catch (Exception ex)
        {
            App.Log("Ball", ex);
            if (_subclassGc.IsAllocated) _subclassGc.Free();
        }
    }

    /// <summary>首次定位/拓扑自愈重摆：按上次落盘模式恢复（贴边 → 贴边收起；自由悬浮 → 恢复坐标常显）。
    /// 目标显示器由保存状态推导：贴边保存的是展开贴边位置，与自由悬浮一样取「保存位置左上角 + 1px」
    /// 探测——该点必在目标屏工作区内且不依赖窗口尺寸（未初始化 _x/_y 也能算）；
    /// 旧数据（未存位置）回退光标所在屏。缩放优先取目标屏 DPI（混合 DPI 下光标与球不在同屏时
    /// 不被带偏），Loaded 校正拿到 XamlRoot 真实缩放后以 knownScale 传入覆盖。</summary>
    private void PlaceInitial(double? knownScale = null)
    {
        _docked = LocalState.Ui.BallDocked;
        _side = LocalState.Ui.BallEdge switch
        {
            "left" or "top" or "bottom" => LocalState.Ui.BallEdge,
            _ => "right",
        };

        // 「未记录」哨兵是 -1，必须精确比较：屏幕坐标是虚拟屏幕空间，主屏左侧/上方副屏的
        // 合法坐标可能为负（如左贴边 BallX = -1920），用 >= 0 判定会把它们误当未记录
        var (px, py) = LocalState.Ui.BallX != -1 && LocalState.Ui.BallY != -1
            ? (LocalState.Ui.BallX + 1, LocalState.Ui.BallY + 1)
            : GetCursorPosition();
        if (!GetWorkAreaAt(px, py, out var work))
        {
            // 兜底主屏（(0,0) 必然属于主屏；GetMonitorInfo 几乎不可能失败）。
            // 不回退就不摆位，窗口会以默认大窗尺寸显示在角落——比球体错位更糟。
            if (!GetWorkAreaAt(0, 0, out work)) return;
        }

        var scale = knownScale ?? GetDpiScaleAt(px, py);
        _scale = scale;
        _w = (int)Math.Round(BallSizeDip * scale);
        _h = _w;

        if (_docked)
        {
            var x = LocalState.Ui.BallX;
            var y = LocalState.Ui.BallY;
            if (x == -1) x = work.Left + (work.Right - work.Left - _w) / 2;   // 旧数据：水平居中
            if (y == -1) y = work.Top + (work.Bottom - work.Top - _h) / 4;
            _x = ClampX(x, work);   // 先按保存坐标夹紧（上下贴边保留水平位）
            _y = ClampY(y, work);   // （左右贴边保留垂直位）
            var (cx, cy) = DockedCollapsed(work);
            _x = cx;
            _y = cy;
            _expanded = false;  // 贴边初始收起
        }
        else
        {
            var x = LocalState.Ui.BallX;
            var y = LocalState.Ui.BallY;
            if (x == -1) x = work.Left + (work.Right - work.Left - _w) / 2;
            if (y == -1) y = work.Top + (work.Bottom - work.Top - _h) / 4;
            _x = ClampX(x, work);
            _y = ClampY(y, work);
            _expanded = true;  // 自由悬浮恒全显
        }
        SetPos(_x, _y, _w, _h);  // 尺寸随位置一起落定（未设置时窗口保持 WinUI 默认大窗）
    }

    /// <summary>贴边模式下的工作区归属。探测点取贴边方向的露出侧（左缘球取右列、右缘球取左列、
    /// 上缘球取下排、下缘球取上排）：贴边收起时窗口几何中心可能落在邻屏/桌面外（如右缘收起时
    /// 中心 &gt; 屏右界），按中心找屏会把球吸到错误的屏。</summary>
    private bool GetWorkArea(out RECT work)
        => GetWorkAreaAt(ExposedProbeX(), ExposedProbeY(), out work);

    /// <summary>贴边方向的露出侧探测 X（水平边贴边取球心列）。</summary>
    private int ExposedProbeX() => _side switch
    {
        "left" => _x + _w - 2,
        "right" => _x + 2,
        _ => _x + _w / 2,  // top/bottom
    };

    /// <summary>贴边方向的露出侧探测 Y（垂直边贴边取球心行）。</summary>
    private int ExposedProbeY() => _side switch
    {
        "top" => _y + _h - 2,
        "bottom" => _y + 2,
        _ => _y + _h / 2,  // left/right
    };

    /// <summary>任意探测点所在显示器的工作区（自由悬浮落位/恢复用，按球心探测）。</summary>
    private static bool GetWorkAreaAt(int px, int py, out RECT work)
    {
        var hMon = MonitorFromPoint(new POINT { X = px, Y = py }, MONITOR_DEFAULTTONEAREST);
        var mi = new MONITORINFO { cbSize = Marshal.SizeOf<MONITORINFO>() };
        if (hMon != IntPtr.Zero && GetMonitorInfo(hMon, ref mi))
        {
            work = mi.rcWork;
            return true;
        }
        work = default;
        return false;
    }

    /// <summary>贴边收起时的窗口位置：沿边缘方向只露 Sliver 宽的窄条（垂直边贴边外缩到屏外、
    /// 水平边贴边外缩到屏上/屏下），垂直于边缘的方向夹紧并保持当前位。</summary>
    private (int X, int Y) DockedCollapsed(RECT work)
    {
        var sliver = (int)Math.Round(SliverDip * _scale);
        return _side switch
        {
            "left" => (work.Left - (_w - sliver), ClampY(_y, work)),
            "right" => (work.Right - sliver, ClampY(_y, work)),
            "top" => (ClampX(_x, work), work.Top - (_h - sliver)),
            _ => (ClampX(_x, work), work.Bottom - sliver),
        };
    }

    /// <summary>完全露出时的窗口位置：紧贴工作区边缘（垂直边贴边沿边缘水平、水平边贴边垂直）。</summary>
    private (int X, int Y) DockedExpanded(RECT work)
    {
        return _side switch
        {
            "left" => (work.Left, ClampY(_y, work)),
            "right" => (work.Right - _w, ClampY(_y, work)),
            "top" => (ClampX(_x, work), work.Top),
            _ => (ClampX(_x, work), work.Bottom - _h),
        };
    }

    /// <summary>把 X 夹紧到工作区水平范围内。</summary>
    private int ClampX(int x, RECT work)
        => Math.Max(work.Left, Math.Min(x, work.Right - _w));

    /// <summary>把 Y 夹紧到工作区垂直范围内（任务栏之上的可视区）。</summary>
    private int ClampY(int y, RECT work)
        => Math.Max(work.Top, Math.Min(y, work.Bottom - _h));

    /// <summary>移动（+可选改尺寸）窗口。w/h 为 0 时只移动（保持原尺寸）。</summary>
    private void SetPos(int x, int y, int w = 0, int h = 0)
    {
        _x = x;
        _y = y;
        if (_hwnd == IntPtr.Zero) return;
        var flags = SWP_NOZORDER | SWP_NOACTIVATE | (w > 0 && h > 0 ? 0u : SWP_NOSIZE);
        SetWindowPos(_hwnd, IntPtr.Zero, x, y, w, h, flags);
    }

    /// <summary>XamlRoot 缩放变化（跨 DPI 屏拖拽、系统缩放调整）：内容按新缩放重渲染，
    /// 窗口物理尺寸必须跟着重算，否则圆形被裁切、sliver 露出宽度失真。
    /// 拖拽中只改尺寸（位置由增量数学继续）；静止时贴边模式按「离哪个状态近」恢复收起/展开，
    /// 自由悬浮仅把位置夹紧回工作区。</summary>
    private void OnXamlRootChanged(XamlRoot sender, XamlRootChangedEventArgs args)
    {
        var scale = _ball.XamlRoot?.RasterizationScale ?? _scale;
        if (Math.Abs(scale - _scale) < 0.02) return;
        StopSlide();  // 打断进行中的滑出/收起：旧几何算出的动画目标已失效
        _scale = scale;
        _w = (int)Math.Round(BallSizeDip * scale);
        _h = _w;
        if (_hwnd != IntPtr.Zero)
            SetWindowPos(_hwnd, IntPtr.Zero, _x, _y, _w, _h, SWP_NOZORDER | SWP_NOACTIVATE);
        if (_captured || _dragging) return;
        if (!_docked)
        {
            // 自由悬浮：仅夹紧到球心所在工作区（模式与展开态不变）
            if (GetWorkAreaAt(_x + _w / 2, _y + _h / 2, out var fwork))
            {
                _x = ClampX(_x, fwork);
                _y = ClampY(_y, fwork);
                SetPos(_x, _y);
            }
            return;
        }
        if (!GetWorkArea(out var work)) return;
        var (cx, cy) = DockedCollapsed(work);
        var (ex, ey) = DockedExpanded(work);
        _expanded = Math.Abs(_x - cx) + Math.Abs(_y - cy) > Math.Abs(_x - ex) + Math.Abs(_y - ey);
        _x = _expanded ? ex : cx;
        _y = _expanded ? ey : cy;
        SetPos(_x, _y);
    }

    private const uint WM_DISPLAYCHANGE = 0x007E;
    private readonly SUBCLASSPROC _wndProc = BallWndProc;
    private GCHandle _subclassGc;
    private const uint SubclassId = 202;

    /// <summary>窗口子类钩子：显示器拓扑变化（拔屏/改分辨率/任务栏变动）时把球移回可见桌面。
    /// 实例引用经 dwRefData 传 GCHandle（同一时刻至多一个悬浮球，静态回调拿不到实例）。</summary>
    private static IntPtr BallWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam,
        IntPtr uIdSubclass, IntPtr dwRefData)
    {
        if (msg == WM_DISPLAYCHANGE)
        {
            var self = GCHandle.FromIntPtr(dwRefData).Target as FloatingBallWindow;
            self?.DispatcherQueue.TryEnqueue(self.OnDisplayChange);
        }
        return DefSubclassProc(hWnd, msg, wParam, lParam, uIdSubclass, dwRefData);
    }

    /// <summary>显示器拓扑变化后自愈：当前位置已不在任何工作区内（拔屏导致球悬空在
    /// 消失的屏幕上，鼠标够不到）→ 按保存状态重新落位到可见桌面（含自由悬浮坐标恢复）。</summary>
    private void OnDisplayChange()
    {
        try
        {
            if (_captured || _dragging) return;
            if (!GetWorkArea(out var work)) return;
            if (_x + _w - 1 < work.Left || _x > work.Right
                || _y + _h - 1 < work.Top || _y > work.Bottom)
            {
                App.Log("Ball", $"display changed, re-place from ({_x},{_y})");
                PlaceInitial();
            }
        }
        catch (Exception ex) { App.Log("Ball", ex); }
    }

    // ==================== 滑出/收起动画 ====================

    private void StartSlide(int fromX, int fromY, int toX, int toY)
    {
        StopSlide();
        if (fromX == toX && fromY == toY) return;
        _slideFromX = fromX;
        _slideFromY = fromY;
        _slideToX = toX;
        _slideToY = toY;
        _slideStart = Environment.TickCount64;
        _slideTimer.Start();
    }

    private void StopSlide() => _slideTimer.Stop();

    /// <summary>位置逐帧插值（ease-out cubic），只动贴边方向上的坐标轴（左右贴边只动 X，
    /// 上下贴边只动 Y，另一轴恒定）；动画期间窗口照常响应指针。
    /// 完成时按滑动方向落 _expanded——滑出动画未完成前按下仍算「收起态」，
    /// 触摸/快速点击才能触发按下全显（见 OnBallPressed）。</summary>
    private void SlideTick()
    {
        var t = (Environment.TickCount64 - _slideStart) / (double)SlideMs;
        if (t >= 1)
        {
            StopSlide();
            t = 1;
            _expanded = _slideExpanding;
        }
        var eased = 1 - Math.Pow(1 - t, 3);
        var sx = _slideFromX + (int)Math.Round((_slideToX - _slideFromX) * eased);
        var sy = _slideFromY + (int)Math.Round((_slideToY - _slideFromY) * eased);
        SetPos(sx, sy);
    }

    // ==================== 指针交互 ====================

    private void OnBallPressed(object sender, PointerRoutedEventArgs e)
    {
        if (_captured) return;  // 第二指/第二个按键按下：忽略，避免重复偏移与基线错乱
        var pt = e.GetCurrentPoint(_ball);
        // 鼠标左键或触摸按下（触摸无按钮属性，触屏拖拽/点击同样支持）
        if (!pt.Properties.IsLeftButtonPressed && e.Pointer.PointerDeviceType != Microsoft.UI.Input.PointerDeviceType.Touch)
            return;
        _pressDip = _lastDip = pt.Position;
        _dragging = false;
        StopSlide();
        // 贴边收起态按下即全显（防收起状态按下后画面只跟着半截跑）。目标按当前贴边方向的
        // 工作区边缘锚定（GetWorkArea 探测贴边露出侧，收起/滑动中均可靠）；
        // 自由悬浮/已展开/滑出动画中按下 = 零位移 no-op（动画已被 StopSlide 打断，球停在当前位）。
        if (_docked && !_expanded && GetWorkArea(out var work))
        {
            var (tx, ty) = DockedExpanded(work);
            if (tx != _x || ty != _y)
            {
                // 窗口平移后，同一指针的窗口相对坐标会反向变化：基线同步补偿，否则真实
                // 点击的 1-2px 手抖会被误判成拖拽（|Δ| 远超 6px 阈值），点击变吸附。
                var shiftDipX = (tx - _x) / _scale;
                var shiftDipY = (ty - _y) / _scale;
                _pressDip.X -= shiftDipX;
                _pressDip.Y -= shiftDipY;
                _lastDip.X -= shiftDipX;
                _lastDip.Y -= shiftDipY;
                SetPos(tx, ty);
            }
        }
        _expanded = true;
        _captured = _ball.CapturePointer(e.Pointer);
    }

    private void OnBallMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_captured) return;
        var pos = e.GetCurrentPoint(_ball).Position;
        var dx = pos.X - _lastDip.X;
        var dy = pos.Y - _lastDip.Y;
        _lastDip = pos;

        if (!_dragging)
        {
            // 位移未超阈值前不算拖（点击判定）；用从按下点的总位移
            var dx0 = pos.X - _pressDip.X;
            var dy0 = pos.Y - _pressDip.Y;
            if (Math.Sqrt(dx0 * dx0 + dy0 * dy0) < DragThresholdDip) return;
            _dragging = true;
        }

        var scale = _ball.XamlRoot?.RasterizationScale ?? _scale;
        SetPos(_x + (int)Math.Round(dx * scale), _y + (int)Math.Round(dy * scale));
    }

    private void OnBallReleased(object sender, PointerRoutedEventArgs e)
    {
        if (!_captured) return;
        _captured = false;
        _ball.ReleasePointerCapture(e.Pointer);
        var wasDrag = _dragging;
        _dragging = false;
        if (wasDrag)
            EndDrag(e);
        else
            _onToggle();  // 点击：唤起/隐藏主窗口
    }

    /// <summary>指针被系统打断（触摸手掌误触/手势冲突等）：中止本次交互，不触发点击语义。</summary>
    private void OnBallCanceled(object sender, PointerRoutedEventArgs e)
        => AbortDrag(e);

    /// <summary>捕获被系统夺走（Alt+Tab/系统手势等，未走 PointerCanceled）：与取消同处理，
    /// 否则 _captured 残留 true，悬停的 PointerMoved 会被误当拖拽增量移动窗口。</summary>
    private void OnBallCaptureLost(object sender, PointerRoutedEventArgs e)
        => AbortDrag(e);

    /// <summary>中断交互的公共收尾：拖拽中则按松手规则落位（贴边/悬浮 + 夹紧 + 落盘），
    /// 避免球滞留在半途；捕获已丢失时 ReleasePointerCapture 是无害 no-op。</summary>
    private void AbortDrag(PointerRoutedEventArgs e)
    {
        if (!_captured) return;
        _captured = false;
        _ball.ReleasePointerCapture(e.Pointer);
        var wasDrag = _dragging;
        _dragging = false;
        if (wasDrag) EndDrag(e);
    }

    /// <summary>松手/中断落位：按球心所在工作区计算四边距离，离某一边 ≤ DockThresholdDip
    /// （或已滑出边缘，距离为负）→ 贴边驻留；否则自由悬浮常显。位置/模式落盘。
    /// 贴边后松手点已在球外（快速甩动 + 窗口平移到边缘常见）→ 不等 PointerExited，立即收起；
    /// 在球上则保持展开。</summary>
    private void EndDrag(PointerRoutedEventArgs? e)
    {
        if (!GetWorkAreaAt(_x + _w / 2, _y + _h / 2, out var work)) return;
        var distLeft = _x - work.Left;
        var distRight = work.Right - (_x + _w);
        var distTop = _y - work.Top;
        var distBottom = work.Bottom - (_y + _h);
        var threshold = (int)Math.Round(DockThresholdDip * _scale);
        var min = Math.Min(Math.Min(distLeft, distRight), Math.Min(distTop, distBottom));

        if (min <= threshold)
        {
            // 贴边驻留：选最近的工作区边缘，垂直于边缘的位置夹紧
            _side = min == distLeft ? "left"
                : min == distRight ? "right"
                : min == distTop ? "top" : "bottom";
            var (tx, ty) = DockedExpanded(work);
            // 松手点相对窗口的坐标会随窗口平移反向变化：换算到平移后的窗口再判定是否在球上
            var pointerOnBall = true;
            if (e is not null)
            {
                var p = e.GetCurrentPoint(_ball).Position;
                var relX = p.X - (tx - _x) / _scale;
                var relY = p.Y - (ty - _y) / _scale;
                var r = BallSizeDip / 2;
                // +1 容忍 DIP/物理像素换算的取整误差
                pointerOnBall = (relX - r) * (relX - r) + (relY - r) * (relY - r) <= r * r + 1;
            }
            _docked = true;
            _expanded = pointerOnBall;
            _x = tx;
            _y = ty;
            SetPos(tx, ty);
            if (!pointerOnBall)
            {
                // 松手点已在球外：不等 PointerExited，直接收起（双保险）
                var (cx, cy) = DockedCollapsed(work);
                _x = cx;
                _y = cy;
                SetPos(cx, cy);
            }
            LocalState.Ui.BallEdge = _side;
            LocalState.Ui.BallDocked = true;
            LocalState.Ui.BallX = tx;  // 展开贴边位置：重启恢复时以此探测目标屏（含 top/bottom 水平位）
            LocalState.Ui.BallY = ty;
        }
        else
        {
            // 自由悬浮：常显全圆；位置夹紧（阈值外夹紧是 no-op，双保险）
            _docked = false;
            _expanded = true;
            _x = ClampX(_x, work);
            _y = ClampY(_y, work);
            SetPos(_x, _y);
            LocalState.Ui.BallDocked = false;
            LocalState.Ui.BallX = _x;
            LocalState.Ui.BallY = _y;
        }
        LocalState.SaveUi();
    }

    private void OnBallEntered(object sender, PointerRoutedEventArgs e)
    {
        if (_captured || _dragging || !_docked) return;  // 自由悬浮不做滑出动画
        StopSlide();
        _slideExpanding = true;
        if (GetWorkArea(out var work))
        {
            var (tx, ty) = DockedExpanded(work);
            StartSlide(_x, _y, tx, ty);
        }
    }

    private void OnBallExited(object sender, PointerRoutedEventArgs e)
    {
        if (_captured || _dragging || !_docked) return;  // 自由悬浮不收起
        StopSlide();
        _slideExpanding = false;
        _expanded = false;
        if (GetWorkArea(out var work))
        {
            var (tx, ty) = DockedCollapsed(work);
            StartSlide(_x, _y, tx, ty);
        }
    }

    private void OnBallRightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        var menu = new MenuFlyout();
        var open = new MenuFlyoutItem { Text = "打开主界面", Icon = new FontIcon { Glyph = "\uE8F1" } };
        open.Click += (_, _) => _onShow();
        var exit = new MenuFlyoutItem { Text = "退出 Spark", Icon = new FontIcon { Glyph = "\uE7E8" } };
        exit.Click += (_, _) => _onExit();
        menu.Items.Add(open);
        menu.Items.Add(new MenuFlyoutSeparator());
        menu.Items.Add(exit);
        menu.ShowAt(_ball, new FlyoutShowOptions { Position = e.GetPosition(_ball) });
    }

    // ==================== 视觉 ====================

    /// <summary>主题切换：圆底/描边颜色随主窗口主题（由 MainWindow.ApplyTheme 同步调用）。</summary>
    public void ApplyTheme(bool dark)
    {
        _ball.Background = dark ? DarkBg : LightBg;
        _ball.BorderBrush = dark ? DarkBorder : LightBorder;
        // 右键菜单/提示等系统控件跟随应用主题（独立窗口不继承主窗口的 RequestedTheme）
        _ball.RequestedTheme = dark ? ElementTheme.Dark : ElementTheme.Light;
    }

    private static FrameworkElement CreateIcon()
    {
        // 优先专用圆形图标（Assets/ball.png，随构建复制到输出目录）：不透明深色正圆底 +
        // 严格居中的白色四芒星与两个点缀圆点，保证悬浮球是正圆、logo 垂直+水平居中；
        // 缺失时回退旧版圆角方块图标（spark.png，需留边距再被圆底裁切），再回退字形
        foreach (var (name, margin) in new[] { ("ball.png", 0.0), ("spark.png", 11.0) })
        {
            var png = Path.Combine(AppContext.BaseDirectory, "Assets", name);
            if (File.Exists(png))
            {
                try
                {
                    return new Image
                    {
                        Source = new BitmapImage(new Uri(png)),
                        Stretch = Stretch.Uniform,
                        Margin = new Thickness(margin),
                    };
                }
                catch { /* 资源缺失回退下一个 */ }
            }
        }
        return new FontIcon
        {
            Glyph = "\uE734",
            FontSize = 20,
            Foreground = new SolidColorBrush(Color.FromArgb(0xFF, 0x0A, 0x84, 0xFF)),
        };
    }

    // ==================== 生命周期 ====================

    /// <summary>开关关闭/应用退出时销毁（设置开关每次重建，WinUI 支持同进程多窗口）。</summary>
    public void Dispose()
    {
        try { _slideTimer.Stop(); } catch { /* ignore */ }
        if (_subclassGc.IsAllocated)
        {
            // 仅移除成功才释放 GCHandle：若子类仍挂着而 dwRefData 已释放，
            // 下一次 WM_DISPLAYCHANGE 的 FromIntPtr.Target 会抛 InvalidOperationException
            var removed = false;
            try { removed = RemoveWindowSubclass(_hwnd, _wndProc, new UIntPtr(SubclassId)); }
            catch { /* ignore */ }
            if (removed) _subclassGc.Free();
        }
        try { Close(); } catch { /* ignore */ }
    }

    // ==================== P/Invoke ====================

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);

    [DllImport("user32.dll")]
    private static extern int GetWindowLong(IntPtr hWnd, int nIndex);

    [DllImport("user32.dll")]
    private static extern int SetWindowLong(IntPtr hWnd, int nIndex, int dwNewLong);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINT lpPoint);

    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromPoint(POINT pt, uint dwFlags);

    [DllImport("user32.dll")]
    private static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFO lpmi);

    [DllImport("shcore.dll")]
    private static extern int GetDpiForMonitor(IntPtr hmonitor, int dpiType, out uint dpiX, out uint dpiY);

    // SetWindowPos 标志
    private const uint SWP_NOSIZE = 0x0001;
    private const uint SWP_NOMOVE = 0x0002;
    private const uint SWP_NOZORDER = 0x0004;
    private const uint SWP_NOACTIVATE = 0x0010;
    private const uint SWP_FRAMECHANGED = 0x0020;

    // MonitorFromPoint：找不到包含点时返回最近显示器
    private const uint MONITOR_DEFAULTTONEAREST = 0x00000002;

    // 窗口子类（显示器拓扑变化自愈，见 BallWndProc）
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

    /// <summary>光标当前位置（探测点回退用：旧数据未存位置时，球恢复到光标所在屏）。</summary>
    private static (int X, int Y) GetCursorPosition()
    {
        GetCursorPos(out var pt);
        return (pt.X, pt.Y);
    }

    /// <summary>目标显示器（探测点所在屏）的有效 DPI 缩放；失败回退 1.0（Loaded 会按 XamlRoot 校正）。</summary>
    private static double GetDpiScaleAt(int px, int py)
    {
        try
        {
            var hMon = MonitorFromPoint(new POINT { X = px, Y = py }, MONITOR_DEFAULTTONEAREST);
            if (hMon == IntPtr.Zero) return 1.0;
            GetDpiForMonitor(hMon, 0 /*MDT_EFFECTIVE_DPI*/, out var dpi, out _);
            return dpi > 0 ? dpi / 96.0 : 1.0;
        }
        catch { return 1.0; }
    }

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
}
