using System.Runtime.InteropServices;

namespace Spark.UI.Services;

/// <summary>系统通知区域（任务栏旁 ▲ 托盘）图标。正式版也可迁到 Host。</summary>
public sealed class TrayService : IDisposable
{
    private readonly IntPtr _hwnd;
    private readonly Action _onShow;
    private readonly Action _onExit;
    private NOTIFYICONDATA _data;
    private bool _added;
    private IntPtr _hIcon;
    private IntPtr _hMenu;
    private WindowProc? _newProc;
    private IntPtr _oldProc;

    private const int WM_TRAY = 0x0400 + 1;
    private const int WM_LBUTTONUP = 0x0202;
    private const int WM_RBUTTONUP = 0x0205;
    private const int WM_COMMAND = 0x0111;
    private const uint NIF_MESSAGE = 0x01;
    private const uint NIF_ICON = 0x02;
    private const uint NIF_TIP = 0x04;
    private const uint NIM_ADD = 0;
    private const uint NIM_DELETE = 2;
    private const uint MF_STRING = 0;
    private const uint MF_SEPARATOR = 0x800;
    private const int ID_SHOW = 1001;
    private const int ID_EXIT = 1002;

    public TrayService(IntPtr hwnd, string iconPath, Action onShow, Action onExit)
    {
        _hwnd = hwnd;
        _onShow = onShow;
        _onExit = onExit;

        if (File.Exists(iconPath))
            _hIcon = LoadImage(IntPtr.Zero, iconPath, 1 /*IMAGE_ICON*/, 0, 0, 0x10 | 0x40); // LR_LOADFROMFILE|LR_DEFAULTSIZE
        if (_hIcon == IntPtr.Zero)
            _hIcon = LoadIcon(IntPtr.Zero, (IntPtr)32512); // IDI_APPLICATION

        _data = new NOTIFYICONDATA
        {
            cbSize = (uint)Marshal.SizeOf<NOTIFYICONDATA>(),
            hWnd = hwnd,
            uID = 1,
            uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage = WM_TRAY,
            hIcon = _hIcon,
            szTip = "Spark"
        };

        _added = Shell_NotifyIcon(NIM_ADD, ref _data);

        _hMenu = CreatePopupMenu();
        AppendMenu(_hMenu, MF_STRING, (UIntPtr)ID_SHOW, "显示 Spark");
        AppendMenu(_hMenu, MF_SEPARATOR, UIntPtr.Zero, string.Empty);
        AppendMenu(_hMenu, MF_STRING, (UIntPtr)ID_EXIT, "退出");

        // 子类化窗口以收托盘消息（WinUI 不直接暴露 WndProc）
        _newProc = WndProc;
        _oldProc = SetWindowLongPtr(hwnd, -4 /*GWL_WNDPROC*/, Marshal.GetFunctionPointerForDelegate(_newProc));
    }

    private IntPtr WndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (msg == WM_TRAY)
        {
            var mouse = lParam.ToInt32();
            if (mouse == WM_LBUTTONUP)
            {
                _onShow();
                return IntPtr.Zero;
            }
            if (mouse == WM_RBUTTONUP)
            {
                GetCursorPos(out var pt);
                SetForegroundWindow(hWnd);
                var cmd = (int)TrackPopupMenu(_hMenu, 0x0100 /*TPM_RETURNCMD*/, pt.X, pt.Y, 0, hWnd, IntPtr.Zero);
                if (cmd == ID_SHOW) _onShow();
                else if (cmd == ID_EXIT) _onExit();
                return IntPtr.Zero;
            }
        }
        else if (msg == WM_COMMAND)
        {
            var id = wParam.ToInt32() & 0xFFFF;
            if (id == ID_SHOW) { _onShow(); return IntPtr.Zero; }
            if (id == ID_EXIT) { _onExit(); return IntPtr.Zero; }
        }

        return CallWindowProc(_oldProc, hWnd, msg, wParam, lParam);
    }

    public void Dispose()
    {
        if (_added)
        {
            Shell_NotifyIcon(NIM_DELETE, ref _data);
            _added = false;
        }
        if (_oldProc != IntPtr.Zero && _hwnd != IntPtr.Zero)
        {
            SetWindowLongPtr(_hwnd, -4, _oldProc);
            _oldProc = IntPtr.Zero;
        }
        if (_hMenu != IntPtr.Zero) { DestroyMenu(_hMenu); _hMenu = IntPtr.Zero; }
        if (_hIcon != IntPtr.Zero) { DestroyIcon(_hIcon); _hIcon = IntPtr.Zero; }
    }

    private delegate IntPtr WindowProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NOTIFYICONDATA
    {
        public uint cbSize;
        public IntPtr hWnd;
        public uint uID;
        public uint uFlags;
        public uint uCallbackMessage;
        public IntPtr hIcon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)]
        public string szTip;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X; public int Y; }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern bool Shell_NotifyIcon(uint dwMessage, ref NOTIFYICONDATA lpData);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr LoadImage(IntPtr hInst, string name, uint type, int cx, int cy, uint fuLoad);

    [DllImport("user32.dll")]
    private static extern IntPtr LoadIcon(IntPtr hInstance, IntPtr lpIconName);

    [DllImport("user32.dll")]
    private static extern bool DestroyIcon(IntPtr hIcon);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreatePopupMenu();

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool AppendMenu(IntPtr hMenu, uint uFlags, UIntPtr uIDNewItem, string lpNewItem);

    [DllImport("user32.dll")]
    private static extern bool DestroyMenu(IntPtr hMenu);

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINT lpPoint);

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern uint TrackPopupMenu(IntPtr hMenu, uint uFlags, int x, int y, int nReserved, IntPtr hWnd, IntPtr prcRect);

    [DllImport("user32.dll")]
    private static extern IntPtr CallWindowProc(IntPtr lpPrevWndFunc, IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

    private static IntPtr SetWindowLongPtr(IntPtr hWnd, int nIndex, IntPtr dwNewLong)
    {
        if (IntPtr.Size == 8)
            return SetWindowLongPtr64(hWnd, nIndex, dwNewLong);
        return new IntPtr(SetWindowLong32(hWnd, nIndex, dwNewLong.ToInt32()));
    }

    [DllImport("user32.dll", EntryPoint = "SetWindowLong")]
    private static extern int SetWindowLong32(IntPtr hWnd, int nIndex, int dwNewLong);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtr")]
    private static extern IntPtr SetWindowLongPtr64(IntPtr hWnd, int nIndex, IntPtr dwNewLong);
}
