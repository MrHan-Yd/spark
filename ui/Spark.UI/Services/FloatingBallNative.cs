using System.Runtime.InteropServices;

namespace Spark.UI.Services;

/// <summary>
/// 悬浮球窗口的全部 Win32/GDI/WIC 互操作（P/Invoke）与纯托管的小工具函数。
/// 集中在一个地方便于审计签名与资源生命周期：所有 HANDLE 都只在本类内创建/释放，
/// 或由调用方显式负责（参见 FloatingBallWindow.EnsureSurface/DestroySurface）。
/// </summary>
internal static partial class Native
{
    // ---- 窗口类 ----
    public static readonly string WindowClassName = "SparkFloatingBall_" + Environment.ProcessId.ToString("x");
    public static readonly WndProcDelegate WndProcRef = FloatingBallWindow.WndProc;
    private static ushort _classAtom;
    private static readonly object ClassLock = new();

    /// <summary>注册悬浮球窗口类（进程内至多执行一次；失败返回 false，调用方走兜底）。</summary>
    public static bool EnsureWindowClass()
    {
        if (_classAtom != 0) return true;
        lock (ClassLock)
        {
            if (_classAtom != 0) return true;
            var wc = new WNDCLASSEX
            {
                cbSize = Marshal.SizeOf<WNDCLASSEX>(),
                style = 0,
                lpfnWndProc = WndProcRef,
                hInstance = GetModuleHandle(null),
                hCursor = LoadCursor(IntPtr.Zero, IDC_ARROW),
                lpszClassName = WindowClassName,
            };
            var atom = RegisterClassEx(ref wc);
            if (atom == 0)
            {
                // 类已存在（同一进程重复注册）不算失败
                atom = GetClassInfoEx(GetModuleHandle(null), WindowClassName, out _);
            }
            _classAtom = atom;
            return atom != 0;
        }
    }

    /// <summary>创建悬浮球顶层窗口（构造后立即可用；显示与否由调用方决定）。</summary>
    public static IntPtr CreateBallWindow(int x, int y, int w, int h)
    {
        if (!EnsureWindowClass()) return IntPtr.Zero;
        return CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            WindowClassName,
            "Spark",
            WS_POPUP,
            x, y, w, h,
            IntPtr.Zero, IntPtr.Zero,
            GetModuleHandle(null),
            IntPtr.Zero);
    }

    // ---- 常量 ----
    public const int WS_POPUP = unchecked((int)0x80000000);
    public const int WS_EX_LAYERED = 0x00080000;
    public const int WS_EX_TOOLWINDOW = 0x00000080;
    public const int WS_EX_TOPMOST = 0x00000008;
    public const int WS_EX_NOACTIVATE = 0x08000000;

    public const uint SWP_NOSIZE = 0x0001;
    public const uint SWP_NOMOVE = 0x0002;
    public const uint SWP_NOZORDER = 0x0004;
    public const uint SWP_NOACTIVATE = 0x0010;

    public const uint SW_SHOWNOACTIVATE = 4;

    public const int HTCLIENT = 1;
    public const int HTTRANSPARENT = -1;
    public const int MA_NOACTIVATE = 3;

    public const int IDC_ARROW = 32512;

    // WM_NCHITTEST 用，ScreenToClient 需窗口坐标
    public const uint WM_NCHITTEST = 0x0084;
    public const uint WM_MOUSEACTIVATE = 0x0021;
    public const uint WM_LBUTTONDOWN = 0x0201;
    public const uint WM_LBUTTONUP = 0x0202;
    public const uint WM_RBUTTONDOWN = 0x0204;
    public const uint WM_RBUTTONUP = 0x0205;
    public const uint WM_MOUSEMOVE = 0x0200;
    public const uint WM_MOUSELEAVE = 0x02A3;
    public const uint WM_CAPTURECHANGED = 0x0215;
    public const uint WM_DPICHANGED = 0x02E0;
    public const uint WM_DISPLAYCHANGE = 0x007E;
    public const uint WM_NCDESTROY = 0x0082;

    // ---- 消息参数解析 ----
    public static int GetXFromParam(IntPtr l) => unchecked((short)(l.ToInt64() & 0xFFFF));
    public static int GetYFromParam(IntPtr l) => unchecked((short)((l.ToInt64() >> 16) & 0xFFFF));
    public static int GetDpiFromParam(IntPtr w) => (int)(w.ToInt64() & 0xFFFF);

    // ---- 屏幕/工作区 ----
    private const uint MONITOR_DEFAULTTONEAREST = 2;

    public static (int X, int Y) GetCursorPosition()
    {
        GetCursorPos(out var pt);
        return (pt.X, pt.Y);
    }

    /// <summary>任意探测点所在显示器的工作区（自由悬浮落位/恢复用，按球心探测）。</summary>
    public static bool GetWorkAreaAt(int px, int py, out RECT work)
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

    /// <summary>目标显示器（探测点所在屏）的有效 DPI 缩放；失败回退 1.0。</summary>
    public static double GetDpiScaleAt(int px, int py)
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

    // ---- 菜单 ----
    public const uint MF_STRING = 0x0000;
    public const uint MF_SEPARATOR = 0x0800;
    public const uint TPM_RETURNCMD = 0x0100;
    public const uint TPM_RIGHTBUTTON = 0x0002;

    /// <summary>弹出右键菜单并返回所选命令 ID（TPM_RETURNCMD 同步返回，无需 WM_COMMAND）。</summary>
    public static int ShowContextMenu(IntPtr owner, int x, int y, params (string? Text, int Id)[] items)
    {
        var menu = CreatePopupMenu();
        if (menu == IntPtr.Zero) return 0;
        try
        {
            for (int i = 0; i < items.Length; i++)
            {
                if (items[i].Text is null)
                    AppendMenu(menu, MF_SEPARATOR, UIntPtr.Zero, null);
                else
                    AppendMenu(menu, MF_STRING, (UIntPtr)items[i].Id, items[i].Text);
            }
            return TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, owner);
        }
        finally { DestroyMenu(menu); }
    }

    // ---- 像素缩放 ----
    /// <summary>预乘 BGRA 的双线性缩放。预乘空间下颜色可像 alpha 一样线性插值，
    /// 边缘像素过渡正确（半透明渐变），这是圆边平滑的数学基础。</summary>
    public static void ScaleBgra(byte[] src, int sw, int sh, byte[] dst, int dw, int dh)
    {
        if (dst.Length < dw * dh * 4 || src.Length < sw * sh * 4) return;
        int sxMax = sw - 1, syMax = sh - 1;
        for (int y = 0; y < dh; y++)
        {
            var fy = (y * sh / (double)dh) - 0.5;            // 对齐目标像素中心
            var y0 = (int)Math.Clamp(Math.Floor(fy), 0, syMax);
            var y1 = (int)Math.Clamp(y0 + 1, 0, syMax);
            var wy = fy - y0;
            for (int x = 0; x < dw; x++)
            {
                var fx = (x * sw / (double)dw) - 0.5;
                var x0 = (int)Math.Clamp(Math.Floor(fx), 0, sxMax);
                var x1 = (int)Math.Clamp(x0 + 1, 0, sxMax);
                var wx = fx - x0;
                var si00 = (y0 * sw + x0) * 4;
                var si01 = (y0 * sw + x1) * 4;
                var si10 = (y1 * sw + x0) * 4;
                var si11 = (y1 * sw + x1) * 4;
                var o = (y * dw + x) * 4;
                var iwx = 1 - wx;
                var iwy = 1 - wy;
                for (int c = 0; c < 4; c++)
                {
                    double v = src[si00 + c] * iwx * iwy
                             + src[si01 + c] * wx * iwy
                             + src[si10 + c] * iwx * wy
                             + src[si11 + c] * wx * wy;
                    dst[o + c] = (byte)Math.Clamp(Math.Round(v), 0, 255);
                }
            }
        }
    }

    /// <summary>预乘 BGRAG 的 source-over 混合（把 src 画到 dst 的指定偏移处）。
    /// 预乘空间：dst = src + dst * (1 - srcA)。用于把图标覆盖在玻璃球上。</summary>
    public static void BlendOver(byte[] dst, int dw, int dh, byte[] src, int sw, int sh, int ox, int oy)
    {
        for (int y = 0; y < sh; y++)
        {
            var dy = oy + y;
            if (dy < 0 || dy >= dh) continue;
            for (int x = 0; x < sw; x++)
            {
                var dx = ox + x;
                if (dx < 0 || dx >= dw) continue;
                var si = (y * sw + x) * 4;
                var a = src[si + 3];
                if (a == 0) continue;
                var di = (dy * dw + dx) * 4;
                var inv = 1 - a / 255.0;
                dst[di] = (byte)Math.Clamp(src[si] + dst[di] * inv, 0, 255);
                dst[di + 1] = (byte)Math.Clamp(src[si + 1] + dst[di + 1] * inv, 0, 255);
                dst[di + 2] = (byte)Math.Clamp(src[si + 2] + dst[di + 2] * inv, 0, 255);
                dst[di + 3] = (byte)Math.Clamp(a + dst[di + 3] * inv, 0, 255);
            }
        }
    }

    // ---- 分层窗口提交 ----
    /// <summary>把内存 DC 里的 32bpp 预乘 BGRA 位图提交给 DWM 与桌面合成。
    /// ULW_ALPHA + AC_SRC_ALPHA = 逐像素 alpha，窗口边缘平滑由位图决定。</summary>
    public static void PresentLayered(IntPtr hwnd, IntPtr memDc, int w, int h)
    {
        var screenDc = GetDC(IntPtr.Zero);
        try
        {
            var dst = new POINT { X = 0, Y = 0 };
            var src = new POINT { X = 0, Y = 0 };
            var size = new SIZE { cx = w, cy = h };
            var blend = new BLENDFUNCTION { BlendOp = AC_SRC_OVER, SourceConstantAlpha = 255, AlphaFormat = AC_SRC_ALPHA };
            UpdateLayeredWindow(hwnd, screenDc, ref dst, ref size, memDc, ref src, 0, ref blend, ULW_ALPHA);
        }
        finally { ReleaseDC(IntPtr.Zero, screenDc); }
    }

    /// <summary>纯移动分层窗口到新屏幕坐标（不重新合成像素，复用已提交的 memDc 表面）。
    /// SetWindowPos 移动 ULW 窗口时 DWM 会双帧合成出旧位置残影（「分裂影子」）；
    /// UpdateLayeredWindow 原子更新位置 + 表面，无残影。注意 pptDst 是屏幕坐标。</summary>
    public static void MoveLayered(IntPtr hwnd, IntPtr memDc, int x, int y, int w, int h)
    {
        if (memDc == IntPtr.Zero) { SetWindowPos(hwnd, IntPtr.Zero, x, y, 0, 0, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE); return; }
        var screenDc = GetDC(IntPtr.Zero);
        try
        {
            var dst = new POINT { X = x, Y = y };
            var src = new POINT { X = 0, Y = 0 };
            var size = new SIZE { cx = w, cy = h };
            var blend = new BLENDFUNCTION { BlendOp = AC_SRC_OVER, SourceConstantAlpha = 255, AlphaFormat = AC_SRC_ALPHA };
            UpdateLayeredWindow(hwnd, screenDc, ref dst, ref size, memDc, ref src, 0, ref blend, ULW_ALPHA);
        }
        finally { ReleaseDC(IntPtr.Zero, screenDc); }
    }

    // ---- GDI 表面 ----
    public static IntPtr CreateCompatibleDC(IntPtr hdc) => CreateCompatibleDC_Internal(IntPtr.Zero);

    public static BITMAPINFO CreateBitmapInfo(int w, int h) => new()
    {
        header = new BITMAPINFOHEADER
        {
            biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>(),
            biWidth = w,
            biHeight = -h,       // top-down 行序（与内存数组一致，省去随时翻转）
            biPlanes = 1,
            biBitCount = 32,
            biCompression = 0,   // BI_RGB
        },
    };

    public static IntPtr CreateDIBSection(IntPtr hdc, ref BITMAPINFO bmi, out IntPtr bits)
        => CreateDIBSection(hdc, ref bmi, 0 /*DIB_RGB_COLORS*/, out bits, IntPtr.Zero, 0);

    // ==================== P/Invoke ====================

    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    public delegate IntPtr WndProcDelegate(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct WNDCLASSEX
    {
        public int cbSize;
        public uint style;
        public WndProcDelegate lpfnWndProc;
        public int cbClsExtra;
        public int cbWndExtra;
        public IntPtr hInstance;
        public IntPtr hIcon;
        public IntPtr hCursor;
        public IntPtr hbrBackground;
        [MarshalAs(UnmanagedType.LPWStr)] public string? lpszMenuName;
        [MarshalAs(UnmanagedType.LPWStr)] public string? lpszClassName;
        public IntPtr hIconSm;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct POINT { public int X; public int Y; }

    [StructLayout(LayoutKind.Sequential)]
    public struct SIZE { public int cx; public int cy; }

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    public struct MONITORINFO
    {
        public int cbSize;      // 必须初始化 = Marshal.SizeOf<MONITORINFO>()，否则 GetMonitorInfo 失败
        public RECT rcMonitor;  // 显示器完整矩形（物理像素）
        public RECT rcWork;     // 工作区（不含任务栏，物理像素）
        public uint dwFlags;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct BITMAPINFOHEADER
    {
        public uint biSize;
        public int biWidth;
        public int biHeight;
        public ushort biPlanes;
        public ushort biBitCount;
        public uint biCompression;
        public uint biSizeImage;
        public int biXPelsPerMeter;
        public int biYPelsPerMeter;
        public uint biClrUsed;
        public uint biClrImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct BITMAPINFO
    {
        public BITMAPINFOHEADER header;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct BLENDFUNCTION
    {
        public byte BlendOp;
        public byte BlendFlags;
        public byte SourceConstantAlpha;
        public byte AlphaFormat;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct TRACKMOUSEEVENT
    {
        public int cbSize;
        public uint dwFlags;
        public IntPtr hwndTrack;
        public int dwHoverTime;
    }

    private const byte AC_SRC_OVER = 0x00;
    private const byte AC_SRC_ALPHA = 0x01;
    private const int ULW_ALPHA = 0x00000002;
    private const uint TME_LEAVE = 0x0002;

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern ushort RegisterClassEx(ref WNDCLASSEX lpWndClass);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern ushort GetClassInfoEx(IntPtr hInstance, string lpClassName, out WNDCLASSEX lpWndClass);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateWindowExW(int dwExStyle, string lpClassName, string lpWindowName,
        int dwStyle, int x, int y, int nWidth, int nHeight, IntPtr hWndParent, IntPtr hMenu,
        IntPtr hInstance, IntPtr lpParam);

    [DllImport("user32.dll")]
    public static extern IntPtr DefWindowProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool DestroyWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, uint nCmdShow);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int x, int y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    public static extern bool GetCursorPos(out POINT lpPoint);

    [DllImport("user32.dll")]
    public static extern IntPtr MonitorFromPoint(POINT pt, uint dwFlags);

    [DllImport("user32.dll")]
    public static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFO lpmi);

    [DllImport("shcore.dll")]
    public static extern int GetDpiForMonitor(IntPtr hmonitor, int dpiType, out uint dpiX, out uint dpiY);

    [DllImport("user32.dll")]
    public static extern bool ScreenToClient(IntPtr hWnd, ref POINT lpPoint);

    [DllImport("user32.dll")]
    public static extern IntPtr SetCapture(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool ReleaseCapture();

    [DllImport("user32.dll")]
    private static extern bool TrackMouseEvent(ref TRACKMOUSEEVENT lpEventTrack);

    /// <summary>登记 TME_LEAVE：光标移出窗口客户区时收到 WM_MOUSELEAVE（悬停滑出判定用）。</summary>
    public static void TrackMouseLeave(IntPtr hwnd)
    {
        var tme = new TRACKMOUSEEVENT
        {
            cbSize = Marshal.SizeOf<TRACKMOUSEEVENT>(),
            dwFlags = TME_LEAVE,
            hwndTrack = hwnd,
            dwHoverTime = 0,
        };
        TrackMouseEvent(ref tme);
    }

    [DllImport("gdi32.dll", EntryPoint = "CreateCompatibleDC")]
    private static extern IntPtr CreateCompatibleDC_Internal(IntPtr hdc);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateDIBSection(IntPtr hdc, ref BITMAPINFO pbmi, uint iUsage,
        out IntPtr ppvBits, IntPtr hSection, uint dwOffset);

    [DllImport("gdi32.dll")]
    public static extern IntPtr SelectObject(IntPtr hdc, IntPtr hObject);

    [DllImport("gdi32.dll")]
    public static extern bool DeleteObject(IntPtr hObject);

    [DllImport("gdi32.dll")]
    public static extern bool DeleteDC(IntPtr hdc);

    [DllImport("user32.dll")]
    public static extern bool UpdateLayeredWindow(IntPtr hWnd, IntPtr hdcDst, ref POINT pptDst, ref SIZE psize,
        IntPtr hdcSrc, ref POINT pptSrc, int crKey, ref BLENDFUNCTION pblend, int dwFlags);

    [DllImport("user32.dll")]
    private static extern IntPtr GetDC(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern int ReleaseDC(IntPtr hWnd, IntPtr hDC);

    [DllImport("user32.dll")]
    private static extern IntPtr LoadCursor(IntPtr hInstance, int lpCursorName);

    [DllImport("kernel32.dll")]
    public static extern IntPtr GetModuleHandle(string? lpModuleName);

    [DllImport("user32.dll")]
    private static extern IntPtr CreatePopupMenu();

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool AppendMenu(IntPtr hMenu, uint uFlags, UIntPtr uIDNewItem, string? lpNewItem);

    [DllImport("user32.dll")]
    private static extern int TrackPopupMenu(IntPtr hMenu, uint uFlags, int x, int y, IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool DestroyMenu(IntPtr hMenu);
}