using System.Collections.ObjectModel;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Spark.UI.Models;
using Spark.UI.Services;
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
    /// <summary>显示后短时忽略失焦，避免 ForceForeground / 热键抢焦导致闪一下就关。</summary>
    private long _ignoreDeactivateUntilTicks;
    /// <summary>合并 event + pipe 双通道重复 toggle。</summary>
    private long _lastToggleTicks;

    public MainWindow()
    {
        try { InitializeComponent(); }
        catch (Exception ex) { App.Log("InitializeComponent", ex); throw; }

        ExtendsContentIntoTitleBar = true;
        ResultList.ItemsSource = _items;
        ResultGrid.ItemsSource = _items;
        LocalState.Load();

        try { SetupChrome(); } catch (Exception ex) { App.Log("SetupChrome", ex); }

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
            BuildFavoriteDock();
            _ = MaintainHostConnectionAsync();
        };
    }

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
            const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
            const int DWMWCP_ROUND = 2;
            var pref = DWMWCP_ROUND;
            DwmSetWindowAttribute(_hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref pref, sizeof(int));
        }
        catch { /* ignore */ }

        PlaceWindow(680, 560);
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
        var area = DisplayArea.GetFromWindowId(_appWindow.Id, DisplayAreaFallback.Primary);
        if (area is null) return;
        var work = area.WorkArea;
        _appWindow.Move(new PointInt32(
            work.X + (work.Width - w) / 2,
            work.Y + Math.Max(80, work.Height / 6)));
    }

    public void ShowLauncher()
    {
        try
        {
            // 先标记可见 + 保护期，再 Show，避免中间 Deactivated 立刻 Hide
            _visible = true;
            _ignoreDeactivateUntilTicks = Environment.TickCount64 + 500;

            PlaceWindow(680, 560);
            if (_hwnd == IntPtr.Zero)
                _hwnd = WindowNative.GetWindowHandle(this);

            try { _appWindow?.Show(true); } catch { /* ignore */ }
            ShowWindow(_hwnd, 9);  // SW_RESTORE
            ShowWindow(_hwnd, 5);  // SW_SHOW

            ForceForeground();
            Activate();
            SettingsPanel.Visibility = Visibility.Collapsed;
            QueryBox.Text = "";
            _ = RefreshResultsAsync("");
            QueryBox.Focus(FocusState.Programmatic);

            // 焦点落稳后再延长一点保护
            _ignoreDeactivateUntilTicks = Environment.TickCount64 + 400;
        }
        catch (Exception ex)
        {
            App.Log("ShowLauncher", ex);
        }
    }

    public void HideLauncher()
    {
        // 保护期内禁止隐藏（防闪关）
        if (Environment.TickCount64 < _ignoreDeactivateUntilTicks && _visible)
            return;

        try { _appWindow?.Hide(); } catch { /* ignore */ }
        try
        {
            if (_hwnd != IntPtr.Zero)
                ShowWindow(_hwnd, 0); // SW_HIDE
        }
        catch { /* ignore */ }
        _visible = false;
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
            item.IconImage = await Task.Run(() => AppIconService.GetIcon(id, hint));
            if (gen != _queryGen) return;
            _items.Add(item);
            i++;
        }

        var hostTag = _host.IsConnected ? "Host · 极速" : "演示 · 本地";
        SearchMeta.Text = _items.Count > 0 ? $"{_items.Count} 项" : "";
        Footer.Text = _items.Count > 0 ? hostTag : "未找到相关结果";
        // 搜索时收藏区变淡（对齐原型 dimmed）
        if (FavIcons.Parent is FrameworkElement favRoot)
            favRoot.Opacity = string.IsNullOrWhiteSpace(q) ? 1.0 : 0.45;

        if (_items.Count > 0)
        {
            _active = 0;
            ResultList.SelectedIndex = 0;
            ResultGrid.SelectedIndex = 0;
        }
    }

    private void BuildFavoriteDock()
    {
        FavIcons.Children.Clear();
        var ids = LocalState.Fav.Items.Select(x => x.ItemId).Distinct().ToList();
        if (ids.Count == 0)
            ids = ["app.wt", "app.code", "app.chrome", "app.explorer", "sys.settings"];

        var n = 0;
        foreach (var id in ids)
        {
            var c = _items.FirstOrDefault(x => x.Id == id) ?? DemoData.Find(id);
            if (c is null) continue;
            n++;

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
                        Text = c.IconGlyph, FontSize = 12, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
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
                Text = c.Title, FontSize = 10, Foreground = new SolidColorBrush(Color.FromArgb(0x8C, 0xFF, 0xFF, 0xFF)),
                TextAlignment = TextAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis,
                MaxLines = 1
            });

            var btn = new Button
            {
                Content = panel,
                Padding = new Thickness(0),
                Background = new SolidColorBrush(Color.FromArgb(0x0A, 0xFF, 0xFF, 0xFF)),
                BorderThickness = new Thickness(1),
                BorderBrush = new SolidColorBrush(Color.FromArgb(0x1A, 0xFF, 0xFF, 0xFF)),
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
                catch (Exception ex) { App.Log("FavInvoke", ex); }
                HideLauncher();
            };
            FavIcons.Children.Add(btn);
        }
        FavCount.Text = n > 0 ? n.ToString() : "";
    }

    private void OnViewList(object sender, RoutedEventArgs e)
    {
        _gridView = false;
        ResultList.Visibility = Visibility.Visible;
        ResultGrid.Visibility = Visibility.Collapsed;
        BtnViewList.Background = new SolidColorBrush(Color.FromArgb(0x38, 0x0A, 0x84, 0xFF));
        BtnViewGrid.Background = new SolidColorBrush(Colors.Transparent);
        ((FontIcon)BtnViewList.Content).Foreground = new SolidColorBrush(Color.FromArgb(0xFF, 0xEB, 0xEB, 0xEB));
        ((FontIcon)BtnViewGrid.Content).Foreground = new SolidColorBrush(Color.FromArgb(0xFF, 0x61, 0xFF, 0xFF));
    }

    private void OnViewGrid(object sender, RoutedEventArgs e)
    {
        _gridView = true;
        ResultList.Visibility = Visibility.Collapsed;
        ResultGrid.Visibility = Visibility.Visible;
        BtnViewGrid.Background = new SolidColorBrush(Color.FromArgb(0x38, 0x0A, 0x84, 0xFF));
        BtnViewList.Background = new SolidColorBrush(Colors.Transparent);
        ((FontIcon)BtnViewGrid.Content).Foreground = new SolidColorBrush(Color.FromArgb(0xFF, 0xEB, 0xEB, 0xEB));
        ((FontIcon)BtnViewList.Content).Foreground = new SolidColorBrush(Color.FromArgb(0xFF, 0x61, 0xFF, 0xFF));
    }

    private void OnOpenSettings(object sender, RoutedEventArgs e)
    {
        SettingsPanel.Visibility = Visibility.Visible;
        AboutText.Text = _host.IsConnected
            ? "Spark UI · 已连接 Host"
            : "Spark UI · 未连接 Host";
    }

    private void OnCloseSettings(object sender, RoutedEventArgs e)
    {
        SettingsPanel.Visibility = Visibility.Collapsed;
        QueryBox.Focus(FocusState.Programmatic);
    }

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
        if (e.Key == VirtualKey.Down && _items.Count > 0)
        {
            e.Handled = true;
            _active = Math.Min(_active + 1, _items.Count - 1);
            SyncSelection();
        }
        else if (e.Key == VirtualKey.Up && _items.Count > 0)
        {
            e.Handled = true;
            _active = Math.Max(_active - 1, 0);
            SyncSelection();
        }
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
        await Task.Delay(60);
        HideLauncher();
    }

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
}
