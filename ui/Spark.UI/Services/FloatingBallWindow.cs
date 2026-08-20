using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Dispatching;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace Spark.UI.Services;

/// <summary>
/// 桌面悬浮球（uTools 同款自绘分层窗口，UpdateLayeredWindow / ULW）：
/// 贴边驻留 + 自由悬浮双模式、悬停滑出 + 拖拽换边/换位、点击唤起主窗口。
///
/// 渲染：不依赖 XAML 画布——程序把 ball.png 按物理尺寸软件合成到一张 32bpp
/// 预乘 alpha 位图里（边缘像素自带半透明过渡 → 圆形轮廓是平滑曲线而非阶梯），
/// 交给 DWM 按像素与桌面混合。窗口不再有「矩形不透明表面」，彻底绕开
/// SetWindowRgn 整数裁剪造成的白/深色背景 1px 锯齿（上一版物理极限）。
///
/// 输入：窗口轮廓的命中测试靠 WM_NCHITTEST（圆内 HTCLIENT / 圆外 HTTRANSPARENT
/// 穿透），鼠标/触摸拖动走 Win32 消息；交互语义（拖拽、贴边、滑出、点击、右键
/// 菜单）与 XAML 版逐行等价移植。
///
/// 位置状态（贴边方向/自由悬浮坐标 + 模式）持久化在 LocalState.Ui，交互约定：
/// - 拖到屏幕边缘（工作区边缘 ≤ DockThresholdDip）→ 贴边驻留：平时只露 SliverDip
///   宽的窄条，鼠标移入滑出全圆、移出滑回收起；
/// - 拖到桌面其他地方 → 自由悬浮：常显全圆，悬停不收起；
/// - 左键按住拖动（位移 &gt; DragThresholdDip 判定为拖，否则为点）；松手按规则落位；
/// - 拖拽被系统打断（WM_CAPTURECHANGED / 手掌误触）→ 按松手规则落位，不触发点击；
/// - 左键点击（无拖动）= 唤起/隐藏主窗口（由 MainWindow 回调决定）；
/// - 右键 = 菜单（打开主界面 / 退出 Spark）。
/// </summary>
public sealed class FloatingBallWindow
{
    private const double BallSizeDip = 46;       // 悬浮球直径（DIP）
    private const double SliverDip = 14;         // 贴边时露出的宽度（DIP）
    private const double DragThresholdDip = 6;   // 位移超过才算拖拽（DIP）
    private const double DockThresholdDip = 16;  // 松手时离工作区边缘小于等于此值 → 贴边驻留（DIP）
    private const int SlideMs = 140;             // 滑出/收起动画时长（ms）
    private const int SlideIntervalMs = 16;      // 动画帧间隔（位置动画用定时器驱动）

    private readonly Action _onToggle;
    private readonly Action _onShow;
    private readonly Action _onExit;
    private readonly DispatcherQueueTimer _slideTimer;
    private IntPtr _hwnd;
    private bool _dark;

    private double _scale = 1.0;
    private int _w, _h;                          // 窗口物理尺寸（正方形）
    private string _side = "right";              // 贴边方向（left/right/top/bottom）
    private int _x, _y;                          // 当前窗口位置（物理像素，屏幕坐标）
    /// <summary>驻留模式：true = 贴边驻留（收起/滑出），false = 自由悬浮（常显全圆，位置任意）。
    /// 由 EndDrag（松手落位）、PlaceInitial、OnDpiChanged 共同维护。</summary>
    private bool _docked;
    /// <summary>当前贴边状态：false = 收起（只露 sliver），true = 全显。
    /// 仅贴边模式有意义（自由悬浮恒为 true）。由 WM_MOUSEMOVE/WM_MOUSELEAVE/按下/缩放重贴边共同维护。</summary>
    private bool _expanded;

    // 指针状态：用屏幕物理像素记录按下点光标位置与窗口位置，拖拽时按绝对位移驱动窗口
    // （newPos = pressWinPos + cursorScreenDelta）。不用客户区坐标算增量——窗口平移后
    // 客户区原点漂移，增量错乱导致「不跟手」+「分裂影子」。
    private bool _captured;
    private bool _expectingCaptureLoss;           // 自己 ReleaseCapture 引起的 WM_CAPTURECHANGED 忽略标记
    private bool _dragging;
    private int _pressScreenX, _pressScreenY;     // 按下时光标屏幕坐标（物理像素）
    private int _pressWinX, _pressWinY;            // 按下时窗口位置（物理像素），拖拽绝对位移基准

    // 滑出/收起动画（只动贴边方向上的坐标轴：左右贴边只动 X，上下贴边只动 Y）
    private int _slideFromX, _slideFromY, _slideToX, _slideToY;
    private long _slideStart;
    /// <summary>当前滑动的目标方向（true = 滑出全显，false = 滑回收起）；动画完成时落 _expanded。</summary>
    private bool _slideExpanding;

    // 渲染表面（按窗口尺寸惰性重建）
    private IntPtr _memDc;
    private IntPtr _dib;
    private IntPtr _oldDib;
    private IntPtr _dibBits;
    private int _surfaceW, _surfaceH;
    private byte[] _pixels = Array.Empty<byte>();

    /// <summary>白色四芒星 glyph（从 spark.png 抠出：亮白像素保留、深色底抠成透明），
    /// 预乘 BGRA + 原始尺寸，静态缓存多实例复用。资源缺失/解码失败为 null —— Render 回退软件绘制。</summary>
    private static byte[]? _starGlyph;
    private static int _glyphW, _glyphH;
    private static readonly object BallLock = new();

    private static readonly ConcurrentDictionary<IntPtr, FloatingBallWindow> Windows = new();

    public FloatingBallWindow(bool dark, Action onToggle, Action onShow, Action onExit)
    {
        _dark = dark;
        _onToggle = onToggle;
        _onShow = onShow;
        _onExit = onExit;

        _slideTimer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        _slideTimer.Interval = TimeSpan.FromMilliseconds(SlideIntervalMs);
        _slideTimer.Tick += (_, _) => SlideTick();

        EnsureBallPixels();

        // 窗口创建后任何一步失败都要销毁 + 清字典，否则留下不可见孤儿窗口（MainWindow
        // 捕获异常后不再引用本实例，窗口句柄无人回收）。
        try
        {
            CreateNativeWindow();
            PlaceInitial();
            Render();      // 先合成第一帧（否则分层窗口无表面 = 不可见）
            Native.ShowWindow(_hwnd, Native.SW_SHOWNOACTIVATE);
        }
        catch
        {
            if (_hwnd != IntPtr.Zero)
            {
                Windows.TryRemove(_hwnd, out _);
                DestroySurface();
                try { Native.DestroyWindow(_hwnd); } catch { /* ignore */ }
                _hwnd = IntPtr.Zero;
            }
            throw;
        }
    }

    // ==================== 原生窗口骨架 ====================

    /// <summary>创建置顶无边框分层窗口：WS_POPUP + WS_EX_LAYERED（像素 alpha 来源）
    /// + WS_EX_TOOLWINDOW（不进任务栏/Alt+Tab）+ WS_EX_TOPMOST（始终置顶）
    /// + WS_EX_NOACTIVATE（点击不抢焦点）。内容完全由 UpdateLayeredWindow 提供，
    /// 窗口没有矩形不透明表面——这就是四角天然透明、圆边平滑的根基。</summary>
    private void CreateNativeWindow()
    {
        _hwnd = Native.CreateBallWindow(0, 0, 100, 100);   // 占位尺寸，PlaceInitial 立即按 DPI 校正
        if (_hwnd == IntPtr.Zero)
            throw new InvalidOperationException("floating ball window creation failed");
        Windows[_hwnd] = this;
    }

    /// <summary>首次定位/拓扑自愈重摆：按上次落盘模式恢复（贴边 → 贴边收起；自由悬浮 → 恢复坐标常显）。
    /// 目标显示器由保存状态推导：贴边保存的是展开贴边位置，与自由悬浮一样取「保存位置左上角 + 1px」
    /// 探测——该点必在目标屏工作区内且不依赖窗口尺寸（未初始化 _x/_y 也能算）；
    /// 旧数据（未存位置）回退光标所在屏。缩放优先取目标屏 DPI（混合 DPI 下光标与球不在同屏时
    /// 不被带偏）。</summary>
    private void PlaceInitial()
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
            : Native.GetCursorPosition();
        if (!Native.GetWorkAreaAt(px, py, out var work))
        {
            // 兜底主屏（(0,0) 必然属于主屏；GetMonitorInfo 几乎不可能失败）。
            // 若连主屏都拿不到，也不能 return——窗口还在占位 (0,0) 会停在左上角；
            // 用 SystemParameters 工作区兜底，至少把球放到可见区。
            Native.GetWorkAreaAt(0, 0, out work);
        }

        _scale = Native.GetDpiScaleAt(px, py);
        if (_scale <= 0) _scale = 1.0;
        _w = (int)Math.Round(BallSizeDip * _scale);
        _h = _w;

        if (_docked)
        {
            var x = LocalState.Ui.BallX;
            var y = LocalState.Ui.BallY;
            if (x == -1) x = work.Right - _w;   // 旧数据：默认右贴边
            if (y == -1) y = work.Top + (work.Bottom - work.Top) / 4;  // 右上角下来一点
            _x = ClampX(x, work);
            _y = ClampY(y, work);
            var (cx, cy) = DockedCollapsed(work);
            _x = cx;
            _y = cy;
            _expanded = false;  // 贴边初始收起
        }
        else
        {
            var x = LocalState.Ui.BallX;
            var y = LocalState.Ui.BallY;
            if (x == -1) x = work.Right - _w;   // 默认右上角
            if (y == -1) y = work.Top + (work.Bottom - work.Top) / 4;  // 右上角下来一点
            _x = ClampX(x, work);
            _y = ClampY(y, work);
            _expanded = true;  // 自由悬浮恒全显
        }
        SetPos(_x, _y, _w, _h);  // 尺寸随位置一起落定（占位 100×100 被覆盖）
    }

    /// <summary>贴边模式下的工作区归属。探测点取贴边方向的露出侧（左缘球取右列、右缘球取左列、
    /// 上缘球取下排、下缘球取上排）：贴边收起时窗口几何中心可能落在邻屏/桌面外（如右缘收起时
    /// 中心 &gt; 屏右界），按中心找屏会把球吸到错误的屏。</summary>
    private bool GetWorkArea(out Native.RECT work)
        => Native.GetWorkAreaAt(ExposedProbeX(), ExposedProbeY(), out work);

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

    /// <summary>贴边收起时的窗口位置：沿边缘方向只露 Sliver 宽的窄条（垂直边贴边外缩到屏外、
    /// 水平边贴边外缩到屏上/屏下），垂直于边缘的方向夹紧并保持当前位。</summary>
    private (int X, int Y) DockedCollapsed(Native.RECT work)
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
    private (int X, int Y) DockedExpanded(Native.RECT work)
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
    private int ClampX(int x, Native.RECT work)
        => Math.Max(work.Left, Math.Min(x, work.Right - _w));

    /// <summary>把 Y 夹紧到工作区垂直范围内（任务栏之上的可视区）。</summary>
    private int ClampY(int y, Native.RECT work)
        => Math.Max(work.Top, Math.Min(y, work.Bottom - _h));

    /// <summary>移动（+可选改尺寸）窗口。w/h 为 0 时只移动（保持原尺寸）。
    /// 分层窗口纯移动用 UpdateLayeredWindow 原子更新位置——SetWindowPos 会留下
    /// 旧位置的合成残影（DWM 双帧合成出「分裂影子」）。尺寸变化时 SetWindowPos
    /// 先改几何（触发 WM_SIZE/重绘），Render 再重新提交表面。</summary>
    private void SetPos(int x, int y, int w = 0, int h = 0)
    {
        _x = x;
        _y = y;
        if (_hwnd == IntPtr.Zero) return;
        if (w > 0 && h > 0)
        {
            // 尺寸变化：SetWindowPos 改窗口几何，ApplyRegion/Render 由调用方负责
            var flags = Native.SWP_NOZORDER | Native.SWP_NOACTIVATE;
            Native.SetWindowPos(_hwnd, IntPtr.Zero, x, y, w, h, flags);
        }
        else
        {
            // 纯移动：UpdateLayeredWindow 同步更新位置与表面（无残影）
            Native.MoveLayered(_hwnd, _memDc, x, y, _w, _h);
        }
    }

    /// <summary>显示器拓扑变化后自愈：当前位置已不在任何工作区内（拔屏导致球悬空在
    /// 消失的屏幕上，鼠标够不到）→ 按保存状态重新落位到可见桌面（含自由悬浮坐标恢复）。</summary>
    private void OnDisplayChange()
    {
        if (_captured || _dragging) return;
        if (!GetWorkArea(out var work)) return;
        if (_x + _w - 1 < work.Left || _x > work.Right
            || _y + _h - 1 < work.Top || _y > work.Bottom)
        {
            App.Log("Ball", $"display changed, re-place from ({_x},{_y})");
            StopSlide();
            _slideExpanding = false;
            PlaceInitial();
            Render();
        }
    }

    // ==================== 消息泵 ====================

    /// <summary>窗口消息回调（静态：经 hwnd → 实例映射分发，单实例或未来多实例都安全）。
    /// 原生消息在此驱动渲染/输入/自愈，全部逻辑在 UI 线程上串行执行。</summary>
    internal static IntPtr WndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (!Windows.TryGetValue(hWnd, out var self))
            return Native.DefWindowProc(hWnd, msg, wParam, lParam);

        switch (msg)
        {
            case Native.WM_NCHITTEST:
                return self.WmNcHitTest(lParam);
            case Native.WM_MOUSEACTIVATE:
                return (IntPtr)Native.MA_NOACTIVATE;   // 点击不激活窗口（不抢前台）
            case Native.WM_LBUTTONDOWN:
                self.OnMouseDown(lParam, left: true);
                return IntPtr.Zero;
            case Native.WM_MOUSEMOVE:
                self.OnMouseMove(lParam);
                return IntPtr.Zero;
            case Native.WM_LBUTTONUP:
                self.OnMouseUp(lParam, left: true);
                return IntPtr.Zero;
            case Native.WM_RBUTTONUP:
                self.OnRightClick();
                return IntPtr.Zero;
            case Native.WM_MOUSELEAVE:
                self.OnMouseLeave();
                return IntPtr.Zero;
            case Native.WM_CAPTURECHANGED:
                self.OnCaptureChanged();
                return IntPtr.Zero;
            case Native.WM_DPICHANGED:
                self.OnDpiChanged(wParam);
                return IntPtr.Zero;
            case Native.WM_DISPLAYCHANGE:
                self.OnDisplayChange();
                return IntPtr.Zero;
            case Native.WM_NCDESTROY:
                self.DestroySurface();
                Windows.TryRemove(hWnd, out _);
                return IntPtr.Zero;
        }
        return Native.DefWindowProc(hWnd, msg, wParam, lParam);
    }

    /// <summary>命中测试：圆内 = HTCLIENT（接收鼠标消息），圆外 = HTTRANSPARENT
    /// （鼠标事件穿透到桌面/下层窗口，四角透明区不挡点击）。这是分层窗口
    /// 取代 SetWindowRgn 裁剪后的命中方案——不再影响渲染，也不产生锯齿。</summary>
    private IntPtr WmNcHitTest(IntPtr lParam)
    {
        var sx = Native.GetXFromParam(lParam);
        var sy = Native.GetYFromParam(lParam);
        var pt = new Native.POINT { X = sx, Y = sy };
        Native.ScreenToClient(_hwnd, ref pt);
        var dx = pt.X - _w / 2.0;
        var dy = pt.Y - _h / 2.0;
        var r = _w / 2.0;
        return (dx * dx + dy * dy <= r * r)
            ? (IntPtr)Native.HTCLIENT
            : (IntPtr)Native.HTTRANSPARENT;
    }

    /// <summary>跨屏 DPI 变化（WM_DPICHANGED，PerMonitorV2 下由系统在窗口换屏时派发）：
    /// 内容按新缩放重合成、窗口物理尺寸重算，否则圆形被裁切、sliver 露出宽度失真。
    /// 拖拽中只改尺寸（位置由增量数学继续）；静止时贴边模式按「离哪个状态近」恢复收起/展开，
    /// 自由悬浮仅把位置夹紧回工作区。交互状态与 XAML 版 OnXamlRootChanged 等价。</summary>
    private void OnDpiChanged(IntPtr wParam)
    {
        var dpi = Native.GetDpiFromParam(wParam);
        if (dpi <= 0) return;
        var scale = dpi / 96.0;
        if (Math.Abs(scale - _scale) < 0.02) return;
        StopSlide();  // 打断进行中的滑出/收起：旧几何算出的动画目标已失效
        _slideExpanding = false;
        _scale = scale;
        _w = (int)Math.Round(BallSizeDip * scale);
        _h = _w;
        SetPos(_x, _y, _w, _h);
        Render();     // 尺寸变化 → 内容重合成
        if (_captured || _dragging) return;
        if (!_docked)
        {
            // 自由悬浮：仅夹紧到球心所在工作区（模式与展开态不变）
            if (Native.GetWorkAreaAt(_x + _w / 2, _y + _h / 2, out var fwork))
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
    /// 快速点击才能触发按下全显（见 OnMouseDown）。</summary>
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

    // ==================== 指针交互（Win32 消息） ====================

    private void OnMouseDown(IntPtr lParam, bool left)
    {
        if (!left) return;             // 右键不参与拖动（关系到菜单，见 OnRightClick）
        if (_captured) return;         // 第二指/第二键按下：忽略，避免重复偏移与基线错乱
        // 用屏幕物理像素记录按下点光标位置与窗口位置，作为拖拽绝对位移的基准
        var (sx, sy) = Native.GetCursorPosition();
        _pressScreenX = sx;
        _pressScreenY = sy;
        _pressWinX = _x;
        _pressWinY = _y;
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
                // 窗口平移到展开位后，窗口基准同步更新（拖拽时按窗口新位算位移，否则首帧跳变）
                _pressWinX = tx;
                _pressWinY = ty;
                SetPos(tx, ty);
            }
        }
        _expanded = true;
        Native.SetCapture(_hwnd);
        _captured = true;
    }

    private void OnMouseMove(IntPtr lParam)
    {
        if (!_captured)
        {
            // 未捕获 = 悬停态：贴边模式下滑出全圆（等价 XAML 的 PointerEntered）；
            // 每次移动都重新装甲 TME_LEAVE，离开时 WM_MOUSELEAVE 触发收起
            if (_docked)
            {
                Native.TrackMouseLeave(_hwnd);
                if (!_expanded && !_slideExpanding && GetWorkArea(out var work))
                {
                    _slideExpanding = true;
                    var (tx, ty) = DockedExpanded(work);
                    StartSlide(_x, _y, tx, ty);
                }
            }
            return;
        }

        // 拖拽：用屏幕物理像素的绝对位移驱动窗口（newPos = pressWinPos + cursorDelta）。
        // 不用客户区坐标增量——窗口一平移客户区原点就漂移，增量错乱导致球跟手偏移
        // 与 DWM 双帧合成出的「分裂影子」。
        var (sx, sy) = Native.GetCursorPosition();
        var dx = sx - _pressScreenX;
        var dy = sy - _pressScreenY;

        if (!_dragging)
        {
            // 位移未超阈值前不算拖（点击判定）
            if (Math.Sqrt(dx * dx + dy * dy) < DragThresholdDip * _scale) return;
            _dragging = true;
        }

        SetPos(_pressWinX + dx, _pressWinY + dy);
    }

    private void OnMouseUp(IntPtr lParam, bool left)
    {
        if (!left || !_captured) return;
        var wasDrag = _dragging;
        _dragging = false;
        var (sx, sy) = Native.GetCursorPosition();
        // 自己释放捕获会同步触发 WM_CAPTURECHANGED：打标记让它忽略，避免把正常松手当捕获被抢
        _expectingCaptureLoss = true;
        Native.ReleaseCapture();
        _captured = false;
        if (wasDrag)
            EndDrag(sx, sy);
        else
            _onToggle();  // 点击：唤起/隐藏主窗口
    }

    /// <summary>捕获被系统夺走（Alt+Tab/触摸被取消/手势冲突，未走正常松手）：像取消一样
    /// 收尾——按松手规则落位，避免球滞留在半途，且不触发点击语义。</summary>
    private void OnCaptureChanged()
    {
        if (_expectingCaptureLoss) { _expectingCaptureLoss = false; return; }
        if (!_captured) return;
        _captured = false;
        var wasDrag = _dragging;
        _dragging = false;
        if (wasDrag)
        {
            var (sx, sy) = Native.GetCursorPosition();
            EndDrag(sx, sy);
        }
    }

    private void OnMouseLeave()
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

    /// <summary>松手/中断落位：按球心所在工作区计算四边距离，离某一边 ≤ DockThresholdDip
    /// （或已滑出边缘，距离为负）→ 贴边驻留；否则自由悬浮常显。位置/模式落盘。
    /// 贴边后松手点已在球外（快速甩动 + 窗口平移到边缘常见）→ 不等 WM_MOUSELEAVE，立即收起；
    /// 在球上则保持展开。releaseScreenX/Y 为松手时光标屏幕物理像素。</summary>
    private void EndDrag(int releaseScreenX, int releaseScreenY)
    {
        if (!Native.GetWorkAreaAt(_x + _w / 2, _y + _h / 2, out var work)) return;
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
            // 松手光标是否在球上：换算到展开后的窗口坐标后做圆形命中测试
            var relX = releaseScreenX - tx;
            var relY = releaseScreenY - ty;
            var r = _w / 2.0;
            var onBall = (relX - r) * (relX - r) + (relY - r) * (relY - r) <= r * r + 1;
            _docked = true;
            _expanded = onBall;
            _x = tx;
            _y = ty;
            SetPos(tx, ty);
            if (!onBall)
            {
                // 松手点已在球外：不等 WM_MOUSELEAVE，直接收起（双保险）
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

    private void OnRightClick()
    {
        // 经典 Win32 弹出菜单（分层窗口不挂 XAML，菜单也走原生路径，行为与 MenuFlyout 等价）
        var (mx, my) = Native.GetCursorPosition();
        var cmd = Native.ShowContextMenu(_hwnd, mx, my,
            ("打开主界面", IdOpenMain),
            (null, 0),   // separator
            ("退出 Spark", IdExit));
        if (cmd == IdOpenMain) _onShow();
        else if (cmd == IdExit) _onExit();
    }

    private const int IdOpenMain = 1;
    private const int IdExit = 2;

    // ==================== 渲染 ====================

    private static void EnsureBallPixels()
    {
        lock (BallLock)
        {
            if (_starGlyph != null) return;
            var (p, w, h) = TryDecodeAsset("spark.png");
            if (p != null)
            {
                // 抠出白色四芒星：亮白像素（Spark 星形）保留，深色圆底透明
                var glyph = new byte[p.Length];
                for (int i = 0; i < p.Length; i += 4)
                {
                    if (p[i + 3] == 0) continue;                       // 原本就透明
                    var lum = p[i] + p[i + 1] + p[i + 2];             // 预乘后亮度
                    if (lum >= 330)                                     // 白星阈值（纯白 765，深底 ~90）
                    {
                        glyph[i] = p[i];
                        glyph[i + 1] = p[i + 1];
                        glyph[i + 2] = p[i + 2];
                        glyph[i + 3] = p[i + 3];
                    }
                    // 深色底座（亮度低于阈值）→ 保持透明
                }
                _starGlyph = glyph;
                _glyphW = w;
                _glyphH = h;
            }
        }
    }

    /// <summary>解码指定 assets 文件为预乘 BGRA 像素与原始尺寸；失败返回 (null,0,0)。</summary>
    private static (byte[]?, int, int) TryDecodeAsset(string name)
    {
        try
        {
            var path = Path.Combine(AppContext.BaseDirectory, "Assets", name);
            if (!File.Exists(path)) return (null, 0, 0);
            var bytes = File.ReadAllBytes(path);
            using var stream = new InMemoryRandomAccessStream();
            stream.WriteAsync(bytes.AsBuffer()).AsTask().GetAwaiter().GetResult();
            stream.Seek(0);
            var decoder = BitmapDecoder.CreateAsync(stream).AsTask().GetAwaiter().GetResult();
            var frame = decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8,
                BitmapAlphaMode.Premultiplied).AsTask().GetAwaiter().GetResult();
            var buf = new byte[frame.PixelWidth * frame.PixelHeight * 4];
            frame.CopyToBuffer(buf.AsBuffer());
            return (buf, frame.PixelWidth, frame.PixelHeight);
        }
        catch (Exception ex) { App.Log("Ball", ex); return (null, 0, 0); }
    }

    /// <summary>合成当前尺寸的画面到 DIB 并提交给 DWM：自绘「玻璃球」——球体径向渐变
    /// （上亮下暗的立体感）+ 左上柔和高光（精灵球光泽）+ 边缘菲涅尔亮边（玻璃折射光感）
    /// + 1px 抗锯齿圆边；中央叠放图标（spark.png 四芒星，悬浮在玻璃内的效果）。
    /// 全部软件合成，最后用 MoveLayered 提交（传当前 _x/_y 保持窗口位置——不能调
    /// PresentLayered，它硬编码 pptDst=(0,0) 会把窗口拽回左上角）。只有尺寸/主题/首次才调用——
    /// 滑出/拖拽只动窗口位置，不重绘，动画流畅度不受影响。</summary>
    private void Render()
    {
        if (_hwnd == IntPtr.Zero || _w <= 0 || _h <= 0) return;
        EnsureSurface(_w, _h);
        var len = _w * _h * 4;
        if (_pixels.Length != len) _pixels = new byte[len];
        Array.Clear(_pixels, 0, len);

        if (_dibBits == IntPtr.Zero)
            return;   // GDI 表面创建失败时保留透明表面（球不显示但进程不崩），下次重试

        DrawGlassBall(_pixels, _w, _h, _dark);
        if (_starGlyph != null)
        {
            // 白色四芒星悬浮在玻璃球内：镂空深底只保留星星本体，约 62% 球径居中
            // （小一点才能透出玻璃高光与渐变，呈现"球里嵌 logo"的漂浮感）
            BlendScaledCentered(_starGlyph, _glyphW, _glyphH, _pixels, _w, _h, 0.62);
        }
        else
            DrawFallbackStar(_pixels, _w);

        Marshal.Copy(_pixels, 0, _dibBits, len);
        Native.MoveLayered(_hwnd, _memDc, _x, _y, _w, _h);
    }

    /// <summary>自绘玻璃球：以球心为原点做归一化像素扫描，逐像素计算
    /// 1) 球体径向渐变底色（光源偏左上 → 右下渐暗，立体感）
    /// 2) 左上柔和高光（白色椭圆高斯衰减，精灵球/玻璃球观感）
    /// 3) 边缘菲涅尔亮圈（玻璃折射拾光，圆边更通透） 4) 底部轻微环境反光
    /// 输出预乘 BGRA。深色主题用暗蓝灰玻璃、浅色主题用银白玻璃。</summary>
    private static void DrawGlassBall(byte[] px, int w, int h, bool dark)
    {
        var c = w / 2.0;
        var r = w / 2.0;
        // 主题色：深色 = 暗蓝灰玻璃；浅色 = 银白玻璃
        byte br, bg, bb, dr2, dg, db;
        if (dark)
        {
            br = 0x6E; bg = 0x76; bb = 0x8C;   // 亮部 #6E768C
            dr2 = 0x10; dg = 0x12; db = 0x1A;  // 暗部 #10121A
        }
        else
        {
            br = 0xFF; bg = 0xFF; bb = 0xFF;   // 亮部 白
            dr2 = 0xB5; dg = 0xBD; db = 0xC8;  // 暗部 #B5BDC8  浅灰蓝
        }
        var s = 1.0 / 255.0;
        // 高光椭圆参数（归一化坐标，光源左上）
        double hx0 = -0.38, hy0 = -0.42, hrx = 0.52, hry = 0.34;
        // 底部环境反光（弧带）
        var bcy = 0.62;
        for (int y = 0; y < h; y++)
        {
            var dy = (y + 0.5 - c) / r;
            for (int x = 0; x < w; x++)
            {
                var dx = (x + 0.5 - c) / r;
                var r2 = dx * dx + dy * dy;
                if (r2 >= 1.0) continue;              // 圆外透明
                var rr = Math.Sqrt(r2);

                // 边缘抗锯齿：圆边界 1px 内 alpha 线性衰减（平滑，绝不阶梯）
                int alpha;
                if (rr > 1 - 1.0 / r)
                    alpha = (int)Math.Round((1 - rr) * r * 255.0);
                else
                    alpha = 255;
                if (alpha <= 0) continue;
                var i = (y * w + x) * 4;

                // 球体径向渐变：距左上光源越近越亮
                var ldx = dx - (-0.35);
                var ldy = dy - (-0.38);
                var lt = Math.Sqrt(ldx * ldx + ldy * ldy) / 1.9;   // 归一化光程
                if (lt > 1) lt = 1;
                var rc = (br - dr2) * (1 - lt) + dr2;   // 红通道
                var gc = (bg - dg) * (1 - lt) + dg;
                var bc = (bb - db) * (1 - lt) + db;

                // 立体阴影：下半球压暗（球体体积感）
                var shade = 1 - 0.22 * Math.Max(0, dy);
                rc *= shade; gc *= shade; bc *= shade;

                // 左上高光：椭圆高斯衰减
                var hx = (dx - hx0) / hrx;
                var hy = (dy - hy0) / hry;
                var hd = hx * hx + hy * hy;
                if (hd < 1)
                {
                    var g = Math.Exp(-hd * 4.0);      // 高斯
                    var wv = 200 * g;                 // 峰值亮度
                    rc += wv; gc += wv; bc += wv;
                }

                // 边缘菲涅尔亮圈：越靠边越拾光（玻璃折射感）
                var fres = 1 - rr;
                if (fres < 1)
                {
                    var f = Math.Pow(1 - fres, 2) * 0.5;   // 边缘强度
                    var fv = 90 * f;
                    rc += fv; gc += fv; bc += fv;
                }

                // 底部环境反光：下方一条柔和亮弧（桌面/环境光反射）
                if (dy > bcy)
                {
                    var bend = (dy - bcy) / (1 - bcy) * 0.5;   // 0..0.5
                    var bv = 60 * bend;
                    rc += bv; gc += bv; bc += bv;
                }

                // 钳制到 0..255 并预乘 alpha。DIB 字节序是 BGRA（蓝、绿、红、alpha），
                // 不是 RGBA——红蓝通道必须反序写入，否则蓝紫玻璃会显示成粉棕
                if (rc > 255) rc = 255; if (gc > 255) gc = 255; if (bc > 255) bc = 255;
                var af = alpha * s;
                px[i] = (byte)(bc * af);
                px[i + 1] = (byte)(gc * af);
                px[i + 2] = (byte)(rc * af);
                px[i + 3] = (byte)alpha;
            }
        }
    }

    /// <summary>把图标资源缩放（双线性）到目标直径的比例后，以球心为基准叠到玻璃球上。
    /// 源是预乘 BGRA，缩放/混合都在预乘空间进行（BlendOver 的 source-over 公式正确）。
    /// scaleOfWindow = 1.0 = 铺满球；&lt;1 = 缩小留边（浅色主题的圆徽章效果）。</summary>
    private static void BlendScaledCentered(byte[] src, int sw, int sh, byte[] dst, int dw, int dh,
        double scaleOfWindow)
    {
        int tw = Math.Max(1, (int)Math.Round(dw * scaleOfWindow));
        int th = Math.Max(1, (int)Math.Round(dh * scaleOfWindow));
        var tmp = new byte[tw * th * 4];
        Native.ScaleBgra(src, sw, sh, tmp, tw, th);
        Native.BlendOver(dst, dw, dh, tmp, tw, th, (dw - tw) / 2, (dh - th) / 2);
    }

    /// <summary>icon 资源缺失时兜底：在玻璃球中央画一个白色四芒星（Spark 品牌元素），
    /// 与 spark.png 构图一致，只是精度更低。</summary>
    private static void DrawFallbackStar(byte[] px, int w)
    {
        var c = w / 2.0;
        var starR = w * 0.30;
        var starC = w * 0.075;
        for (int y = 0; y < w; y++)
        {
            var dy = y + 0.5 - c;
            for (int x = 0; x < w; x++)
            {
                var dx = x + 0.5 - c;
                var i = (y * w + x) * 4;
                if (px[i + 3] == 0) continue;   // 圆外
                var ax = Math.Abs(dx); var ay = Math.Abs(dy);
                var hb = ax / starR + ay / starC;
                var vb = ax / starC + ay / starR;
                if (hb <= 1 || vb <= 1)
                {
                    // 白色星（源over混合，保留原 alpha）
                    var inv = 1 - 255 / 255.0;   // 不透明：直接覆盖
                    px[i] = (byte)(0xF5 + px[i] * inv);
                    px[i + 1] = (byte)(0xF5 + px[i + 1] * inv);
                    px[i + 2] = (byte)(0xF7 + px[i + 2] * inv);
                    px[i + 3] = 255;
                }
            }
        }
    }

    /// <summary>按尺寸惰性创建 32bpp 预乘 BGRA DIB 与配套内存 DC（Render 的提交表面）。
    /// 尺寸不变则复用；变化（DPI 变化）则重建。DIB 用 top-down 行序，和像素数组行序一致。</summary>
    private void EnsureSurface(int w, int h)
    {
        if (_memDc != IntPtr.Zero && _dib != IntPtr.Zero && _surfaceW == w && _surfaceH == h)
            return;
        DestroySurface();
        _memDc = Native.CreateCompatibleDC(IntPtr.Zero);
        var bmi = Native.CreateBitmapInfo(w, h);
        _dib = Native.CreateDIBSection(_memDc, ref bmi, out _dibBits);
        if (_dib != IntPtr.Zero)
        {
            _oldDib = Native.SelectObject(_memDc, _dib);
            _surfaceW = w;
            _surfaceH = h;
        }
    }

    private void DestroySurface()
    {
        if (_memDc != IntPtr.Zero)
        {
            if (_oldDib != IntPtr.Zero) Native.SelectObject(_memDc, _oldDib);
            Native.DeleteDC(_memDc);
        }
        if (_dib != IntPtr.Zero) Native.DeleteObject(_dib);
        _memDc = _dib = _oldDib = IntPtr.Zero;
        _dibBits = IntPtr.Zero;
        _surfaceW = _surfaceH = 0;
    }

    /// <summary>主题切换：深/浅色只影响描边环颜色（球体是 ball.png 本身，两主题共用），
    /// 立即重合成一帧。MainWindow.ApplyTheme 同步调用。</summary>
    public void ApplyTheme(bool dark)
    {
        if (_dark == dark) return;
        _dark = dark;
        Render();
    }

    /// <summary>开关关闭/应用退出时销毁。分层窗口没有 XAML 资源，清理重点是
    /// 定时器、捕获与 GDI 表面；DestroyWindow 的 WM_NCDESTROY 已做字典移除。</summary>
    public void Dispose()
    {
        try { _slideTimer.Stop(); } catch { /* ignore */ }
        if (_captured)
        {
            try { Native.ReleaseCapture(); } catch { /* ignore */ }
            _captured = false;
        }
        if (_hwnd != IntPtr.Zero)
        {
            try { Native.DestroyWindow(_hwnd); } catch { /* ignore */ }
            _hwnd = IntPtr.Zero;
        }
    }
}
