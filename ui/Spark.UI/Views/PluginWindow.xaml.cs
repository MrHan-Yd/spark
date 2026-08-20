using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.UI;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media.Imaging;
using Microsoft.Web.WebView2.Core;
using Spark.UI.Models;
using Spark.UI.Services;
using Windows.Graphics;
using WinRT.Interop;

namespace Spark.UI.Views;

/// <summary>
/// 插件页宿主窗口：一个 WebView2 加载插件的 index.html，注入 <c>window.spark</c>
/// （见《插件开发规范》§8），页面的 spark.* 调用经 postMessage 桥转成
/// <c>host.plugin.api</c>。每个插件单开一个窗口，由 <see cref="PluginWindowHost"/> 管理。
/// </summary>
public sealed partial class PluginWindow : Window
{
    /// <summary>插件资源的虚拟主机名——用 https 源加载，避免 file:// 的同源/fetch 限制。</summary>
    private const string VirtualHost = "plugin.spark.invalid";

    private readonly PluginOpenInfoDto _info;
    private readonly HostIpcClient _host;
    private readonly bool _devMode;
    private readonly AppWindow _appWindow;
    private readonly IntPtr _hwnd;

    private bool _webReady;
    private bool _closing;

    // WM_GETMINMAXINFO 子类化：让用户拖拽边框时也遵守清单 min_width/min_height。
    private readonly SUBCLASSPROC _minMaxProc = MinMaxWndProc;
    private GCHandle _selfPin;
    private int _minTrackW, _minTrackH;  // 物理像素
    private bool _subclassed;
    private const uint PluginSubclassId = 201;

    public string PluginId => _info.Id;

    public PluginWindow(PluginOpenInfoDto info, HostIpcClient host, string input, string command,
        string rawQuery, bool devMode)
    {
        _info = info;
        _host = host;
        _devMode = devMode;

        App.Log("PluginWindow", $"ctor start id={info.Id} name={info.Name} icon_abs={info.IconAbs ?? "(null)"}");
        Dbg($"ctor start id={info.Id} name={info.Name} icon_abs={info.IconAbs ?? "(null)"}");

        InitializeComponent();

        _hwnd = WindowNative.GetWindowHandle(this);
        _appWindow = AppWindow.GetFromWindowId(Win32Interop.GetWindowIdFromWindow(_hwnd));

        TitleText.Text = _info.Name;
        LoadTitleIcon();

        ApplyWindowSpec();
        Dbg("ApplyWindowSpec done");

        Closed += OnClosed;
        _ = InitWebAsync(input, command, rawQuery);
    }

    /// <summary>
    /// 标题栏图标：优先插件清单声明的 icon（host 已拼好绝对路径）；
    /// 读不到或加载异常则降级到 Spark 内置 <c>Assets/spark.png</c>。
    /// 用文件路径而非 ms-appx:///：本项目是 unpackaged self-contained 应用，
    /// Assets 是 Content+CopyToOutput，ms-appx:/// 对任意资源不可靠。
    /// </summary>
    private void LoadTitleIcon()
    {
        if (!TrySetIcon(_info.IconAbs))
            TrySetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "spark.png"));
    }

    private bool TrySetIcon(string? path)
    {
        if (string.IsNullOrEmpty(path) || !File.Exists(path)) return false;
        try
        {
            // 本地反斜杠路径直接 new Uri 会抛 UriFormatException；转成 file:/// URI。
            var file = path.Replace('\\', '/');
            if (!file.StartsWith('/')) file = "/" + file;
            TitleIcon.Source = new BitmapImage(new Uri("file:///" + file));
            return true;
        }
        catch (Exception ex) { App.Log("PluginTitleIcon", ex); return false; }
    }

    private void OnCloseClick(object sender, RoutedEventArgs e) => Close();

    private void OnMinClick(object sender, RoutedEventArgs e)
    {
        if (_appWindow.Presenter is OverlappedPresenter p) p.Minimize();
    }

    private void OnMaxClick(object sender, RoutedEventArgs e) => ToggleMaximize();

    private void ToggleMaximize()
    {
        if (_appWindow.Presenter is not OverlappedPresenter p) return;
        if (IsZoomed(_hwnd)) p.Restore();
        else p.Maximize();
    }

    /// <summary>最大化/还原时切换按钮图标（E922 最大化 / E923 还原）。</summary>
    private void UpdateMaxIcon()
    {
        MaxBtn.Content = IsZoomed(_hwnd) ? "\uE923" : "\uE922";
    }

    private void ApplyWindowSpec()
    {
        var w = _info.Window;
        _appWindow.Title = _info.Name;

        var scale = GetDpiForWindow(_hwnd) / 96.0;
        _appWindow.Resize(new SizeInt32(
            (int)Math.Round(w.Width * scale), (int)Math.Round(w.Height * scale)));

        if (_appWindow.Presenter is OverlappedPresenter p)
        {
            p.IsResizable = w.Resizable;
            p.IsMaximizable = w.Resizable;
            p.IsAlwaysOnTop = w.AlwaysOnTop;
            // 统一去掉系统标题栏：frame=true 时由自绘标题栏接管，frame=false 时插件自绘。
            p.SetBorderAndTitleBar(true, false);
        }

        if (w.Frame)
        {
            // 不可调整大小 → 隐藏最大化按钮（与 IsMaximizable=false 一致）。
            MaxBtn.Visibility = w.Resizable ? Visibility.Visible : Visibility.Collapsed;
            // WinUI 标准自绘标题栏：ExtendsContentIntoTitleBar + SetTitleBar。
            // 标题栏内的 Button 由 WinUI 自动 passthrough（可点击）。
            ExtendsContentIntoTitleBar = true;
            SetTitleBar(TitleBar);
            // 最大化图标随窗口状态切换：用 Root.SizeChanged（UI 线程）而非 AppWindow.Changed（可能跨线程）。
            Root.SizeChanged += (_, _) => UpdateMaxIcon();

            // 用户拖拽边框时强制 min 尺寸：WinAppSDK 1.6 无 AppWindow.MinSize，
            // 用 Win32 WM_GETMINMAXINFO 子类化拦截（与主窗 NoMaximizeWndProc 同款 SetWindowSubclass）。
            if (w.Resizable)
            {
                _minTrackW = (int)Math.Round(w.MinWidth * scale);
                _minTrackH = (int)Math.Round(w.MinHeight * scale);
                try
                {
                    _selfPin = GCHandle.Alloc(this);
                    SetWindowSubclass(_hwnd, _minMaxProc, new UIntPtr(PluginSubclassId),
                        GCHandle.ToIntPtr(_selfPin));
                    _subclassed = true;
                }
                catch (Exception ex) { App.Log("PluginMinMaxSubclass", ex); }
            }
            Dbg($"frame=true setup resizable={w.Resizable} min={_minTrackW}x{_minTrackH}");
        }
        else
        {
            // frame:false → 不显示宿主标题栏，插件页面自绘（规范 §4.3）。
            TitleBarHost.Visibility = Visibility.Collapsed;
        }

        CenterOnScreen();
        Dbg("ApplyWindowSpec done");
    }

    /// <summary>固定路径调试日志，绕过 App.Log（排查 native 崩溃时 App.Log 可能未落盘）。</summary>
    private static void Dbg(string msg)
    {
        try { File.AppendAllText(@"D:\demo\test01\spark\plugin-debug.log",
            $"[{DateTime.Now:HH:mm:ss.fff}] {msg}\n"); }
        catch { }
    }

    private void CenterOnScreen()
    {
        var area = DisplayArea.GetFromWindowId(_appWindow.Id, DisplayAreaFallback.Primary);
        if (area is null) return;
        var size = _appWindow.Size;
        _appWindow.Move(new PointInt32(
            area.WorkArea.X + (area.WorkArea.Width - size.Width) / 2,
            area.WorkArea.Y + (area.WorkArea.Height - size.Height) / 2));
    }

    private async Task InitWebAsync(string input, string command, string rawQuery)
    {
        try
        {
            Dbg("InitWebAsync start");
            await Web.EnsureCoreWebView2Async();
            Dbg("EnsureCoreWebView2Async done");
            var core = Web.CoreWebView2;

            var settings = core.Settings;
            settings.AreDevToolsEnabled = _devMode;
            settings.AreDefaultContextMenusEnabled = _devMode;
            settings.IsStatusBarEnabled = false;
            settings.AreBrowserAcceleratorKeysEnabled = _devMode;

            // 插件根目录映射为虚拟主机；DenyCors 拦住其他站点跨源读插件资源。
            core.SetVirtualHostNameToFolderMapping(
                VirtualHost, _info.Root, CoreWebView2HostResourceAccessKind.DenyCors);

            core.WebMessageReceived += OnWebMessage;
            // 新窗口/导航到外部站点一律拒绝：插件页只能待在自己的虚拟主机内。
            core.NewWindowRequested += (_, e) => e.Handled = true;
            core.NavigationStarting += OnNavigationStarting;

            await core.AddScriptToExecuteOnDocumentCreatedAsync(BuildBootstrapScript(input, command, rawQuery));
            await core.AddScriptToExecuteOnDocumentCreatedAsync(LoadPreloadShim());
            Dbg("preload scripts injected");

            // 自定义 preload.js（规范 §8.4）：在 spark 注入之后执行。
            var custom = ReadCustomPreload();
            if (custom is not null)
                await core.AddScriptToExecuteOnDocumentCreatedAsync(custom);

            _webReady = true;
            core.Navigate(BuildEntryUrl());
            Dbg("Navigate done");
        }
        catch (Exception ex)
        {
            Dbg($"InitWebAsync EX: {ex}");
            App.Log("PluginWebInit", ex);
            Close();
        }
    }

    /// <summary>把 index.html 的绝对路径换算成虚拟主机下的 URL。</summary>
    private string BuildEntryUrl()
    {
        // 逐段转义：分隔符要保留，段内的空格/中文才需要编码。
        var segments = Path.GetRelativePath(_info.Root, _info.MainAbs)
            .Replace('\\', '/')
            .Split('/', StringSplitOptions.RemoveEmptyEntries)
            .Select(Uri.EscapeDataString);
        return $"https://{VirtualHost}/{string.Join('/', segments)}";
    }

    private string BuildBootstrapScript(string input, string command, string rawQuery)
    {
        var boot = JsonSerializer.Serialize(new
        {
            input = new { text = input, command, rawQuery },
            granted = _info.Granted,
            dev = _devMode
        });
        return $"window.__SPARK_BOOTSTRAP__ = {boot};";
    }

    private static string LoadPreloadShim()
    {
        var path = Path.Combine(AppContext.BaseDirectory, "Assets", "plugin.preload.js");
        return File.ReadAllText(path);
    }

    private string? ReadCustomPreload()
    {
        var path = _info.PreloadAbs;
        if (string.IsNullOrEmpty(path)) return null;
        try
        {
            return File.ReadAllText(path);
        }
        catch (Exception ex)
        {
            App.Log("PluginCustomPreload", ex);
            return null;
        }
    }

    /// <summary>只允许留在插件虚拟主机内导航；外链走系统浏览器由 shell.open 权限管（暂不开放）。</summary>
    private void OnNavigationStarting(CoreWebView2 sender, CoreWebView2NavigationStartingEventArgs e)
    {
        if (Uri.TryCreate(e.Uri, UriKind.Absolute, out var uri)
            && uri.Host.Equals(VirtualHost, StringComparison.OrdinalIgnoreCase))
        {
            return;
        }
        e.Cancel = true;
        App.Log("PluginNav", $"blocked {_info.Id} -> {e.Uri}");
    }

    // ─── 消息桥 ──────────────────────────────────────────────────────────

    private async void OnWebMessage(CoreWebView2 sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        int seq = 0;
        try
        {
            using var doc = JsonDocument.Parse(e.WebMessageAsJson);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object
                || !root.TryGetProperty("__spark", out _)
                || !root.TryGetProperty("seq", out var seqEl)) return;

            seq = seqEl.GetInt32();
            var capability = root.GetProperty("capability").GetString() ?? "";
            var method = root.GetProperty("method").GetString() ?? "";
            var args = root.TryGetProperty("args", out var a) ? a.Clone() : default;

            // window/dev 由宿主本地处理，其余转 host。
            var data = capability switch
            {
                "window" => await HandleWindowAsync(method, args),
                "dev" => HandleDev(method),
                _ => await _host.PluginApiAsync(_info.Id, capability, method, args)
            };
            PostReply(seq, true, data, null, null);
        }
        catch (Exception ex)
        {
            var (code, message) = ClassifyError(ex);
            if (seq > 0) PostReply(seq, false, default, code, message);
            App.Log("PluginApi", ex);
        }
    }

    /// <summary>host 以 "CODE: detail" 形式回错误消息，这里还原成规范 §8.6 的 error.code。</summary>
    private static (string Code, string Message) ClassifyError(Exception ex)
    {
        var msg = ex.Message ?? "";
        foreach (var code in new[]
                 {
                     "PERMISSION_DENIED", "PERMISSION_SCOPE", "NETWORK_FAILED",
                     "INVALID_ARGS", "UNAVAILABLE"
                 })
        {
            if (msg.Contains(code, StringComparison.Ordinal)) return (code, msg);
        }
        return ("UNAVAILABLE", msg);
    }

    private void PostReply(int seq, bool ok, JsonElement data, string? code, string? message)
    {
        if (_closing) return;
        var payload = JsonSerializer.Serialize(new
        {
            __spark = 1,
            kind = "reply",
            seq,
            ok,
            data = ok && data.ValueKind != JsonValueKind.Undefined ? data : (JsonElement?)null,
            error = ok ? null : new { code, message }
        });
        TryPostToWeb(payload);
    }

    private void PostEvent(string name, object? payload)
    {
        var json = JsonSerializer.Serialize(new
        {
            __spark = 1, kind = "event", @event = name, payload
        });
        TryPostToWeb(json);
    }

    private void TryPostToWeb(string json)
    {
        try
        {
            Web.CoreWebView2?.PostWebMessageAsJson(json);
        }
        catch (Exception ex)
        {
            App.Log("PluginPost", ex);
        }
    }

    // ─── 宿主本地能力 ────────────────────────────────────────────────────

    private Task<JsonElement> HandleWindowAsync(string method, JsonElement args)
    {
        switch (method)
        {
            case "set_title":
            {
                var title = args.TryGetProperty("title", out var t) ? t.GetString() ?? "" : "";
                _appWindow.Title = title;
                TitleText.Text = title;
                break;
            }

            case "resize":
            {
                var scale = GetDpiForWindow(_hwnd) / 96.0;
                var w = ReadDimension(args, "width", _info.Window.Width, _info.Window.MinWidth);
                var h = ReadDimension(args, "height", _info.Window.Height, _info.Window.MinHeight);
                _appWindow.Resize(new SizeInt32(
                    (int)Math.Round(w * scale), (int)Math.Round(h * scale)));
                PostEvent("resize", new { width = w, height = h });
                break;
            }

            case "center":
                CenterOnScreen();
                break;

            case "close":
                Close();
                break;

            case "set_always_on_top":
            {
                // 权限已在 preload 侧拦一道，这里再按 granted 校验一次（页面可绕过 shim）。
                if (!_info.Granted.Contains("window.alwaysOnTop"))
                    throw new InvalidOperationException("PERMISSION_DENIED: window.alwaysOnTop");
                var on = args.ValueKind == JsonValueKind.True
                         || (args.ValueKind == JsonValueKind.Object
                             && args.TryGetProperty("enabled", out var en)
                             && en.ValueKind == JsonValueKind.True);
                if (_appWindow.Presenter is OverlappedPresenter p) p.IsAlwaysOnTop = on;
                break;
            }

            default:
                throw new InvalidOperationException($"INVALID_ARGS: window method {method}");
        }
        return Task.FromResult(default(JsonElement));
    }

    private static uint ReadDimension(JsonElement args, string name, uint fallback, uint min)
    {
        if (args.ValueKind != JsonValueKind.Object
            || !args.TryGetProperty(name, out var el)
            || !el.TryGetDouble(out var v)
            || double.IsNaN(v))
        {
            return fallback;
        }
        return (uint)Math.Clamp(v, min, 4096);
    }

    private JsonElement HandleDev(string method)
    {
        if (!_devMode) throw new InvalidOperationException("UNAVAILABLE: dev mode off");
        if (method != "open_devtools")
            throw new InvalidOperationException($"INVALID_ARGS: dev method {method}");
        Web.CoreWebView2?.OpenDevToolsWindow();
        return default;
    }

    private void OnClosed(object sender, WindowEventArgs args)
    {
        if (_closing) return;
        _closing = true;
        // 卸载 WM_GETMINMAXINFO 子类化 + 释放实例 pin，避免窗口销毁后回调访问悬空引用。
        if (_subclassed)
        {
            try { RemoveWindowSubclass(_hwnd, _minMaxProc, new UIntPtr(PluginSubclassId)); }
            catch { }
            if (_selfPin.IsAllocated) _selfPin.Free();
            _subclassed = false;
        }
        // 关窗前给页面一次存盘机会（规范 §8.5 onClose）；WebView2 随窗口销毁。
        if (_webReady) PostEvent("close", null);
        try { Web.Close(); } catch (Exception ex) { App.Log("PluginWebClose", ex); }
    }

    /// <summary>被再次触发时聚焦已有窗口并刷新输入上下文（默认单开）。</summary>
    public void FocusWith(string input, string command, string rawQuery)
    {
        _appWindow.Show();
        SetForegroundWindow(_hwnd);
        if (_webReady)
            PostEvent("input", new { text = input, command, rawQuery });
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hwnd);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool IsZoomed(IntPtr hwnd);

    // ─── WM_GETMINMAXINFO 子类化：用户拖拽边框时强制 min 尺寸 ───────────────

    private const uint WM_GETMINMAXINFO = 0x0024;

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X; public int Y; }

    [StructLayout(LayoutKind.Sequential)]
    private struct MINMAXINFO
    {
        public POINT ptReserved;
        public POINT ptMaxSize;
        public POINT ptMaxPosition;
        public POINT ptMinTrackSize;
        public POINT ptMaxTrackSize;
    }

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

    private static IntPtr MinMaxWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam,
        IntPtr uIdSubclass, IntPtr dwRefData)
    {
        if (msg == WM_GETMINMAXINFO && dwRefData != IntPtr.Zero)
        {
            try
            {
                var self = (PluginWindow)GCHandle.FromIntPtr(dwRefData).Target!;
                var mmi = Marshal.PtrToStructure<MINMAXINFO>(lParam);
                mmi.ptMinTrackSize = new POINT { X = self._minTrackW, Y = self._minTrackH };
                Marshal.StructureToPtr(mmi, lParam, false);
            }
            catch { /* min 限制失败不阻断默认行为 */ }
        }
        return DefSubclassProc(hWnd, msg, wParam, lParam, uIdSubclass, dwRefData);
    }
}
