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
using WinRT.Interop;

namespace Spark.UI;

public sealed partial class MainWindow : Window
{
    private readonly ObservableCollection<CandidateDto> _items = new();
    private DispatcherTimer? _debounce;
    private string _view = "list";
    private int _active;
    private bool _loaded;
    private AppWindow? _appWindow;
    private bool _hideOnDeactivate = true;

    public MainWindow()
    {
        InitializeComponent();
        // 内容伸进标题区 → 去掉系统标题栏图标与 - □ ×
        ExtendsContentIntoTitleBar = true;
        ResultList.ItemsSource = _items;
        ResultGrid.ItemsSource = _items;
        SetupLauncherChrome();

        Activated += OnWindowActivated;

        Root.Loaded += async (_, _) =>
        {
            if (_loaded) return;
            _loaded = true;
            QueryBox.Focus(FocusState.Programmatic);
            FillFavorites();
            await SearchAsync("");
        };
    }

    private void SetupLauncherChrome()
    {
        try
        {
            var hwnd = WindowNative.GetWindowHandle(this);
            var id = Win32Interop.GetWindowIdFromWindow(hwnd);
            _appWindow = AppWindow.GetFromWindowId(id);
            _appWindow.Title = "Spark";

            // 无标题栏、不可最大化；保留细边框以便拖/辨认
            if (_appWindow.Presenter is OverlappedPresenter p)
            {
                p.IsResizable = false;
                p.IsMaximizable = false;
                p.IsMinimizable = false;
                p.SetBorderAndTitleBar(true, false);
                p.IsAlwaysOnTop = true;
            }

            // 圆角（Win11）
            try
            {
                _appWindow.TitleBar.ExtendsContentIntoTitleBar = true;
                _appWindow.TitleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
                _appWindow.TitleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
                _appWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Collapsed;
            }
            catch { }

            PlaceWindow(680);
        }
        catch { }
    }

    private void PlaceWindow(int width)
    {
        try
        {
            if (_appWindow is null)
            {
                var hwnd = WindowNative.GetWindowHandle(this);
                var id = Win32Interop.GetWindowIdFromWindow(hwnd);
                _appWindow = AppWindow.GetFromWindowId(id);
            }

            var w = Math.Clamp(width, 560, 840);
            var h = 520;
            _appWindow.Resize(new SizeInt32(w, h));
            var area = DisplayArea.GetFromWindowId(_appWindow.Id, DisplayAreaFallback.Primary);
            if (area is not null)
            {
                var work = area.WorkArea;
                // 屏幕上方居中，类似 Spotlight / uTools
                var x = work.X + (work.Width - w) / 2;
                var y = work.Y + Math.Max(80, work.Height / 6);
                _appWindow.Move(new PointInt32(x, y));
            }
        }
        catch { }
    }

    private void OnWindowActivated(object sender, WindowActivatedEventArgs e)
    {
        if (e.WindowActivationState == WindowActivationState.Deactivated)
        {
            if (_hideOnDeactivate)
                HideLauncher();
            return;
        }

        // 再次显示时聚焦输入
        QueryBox.Focus(FocusState.Programmatic);
    }

    public void ShowLauncher()
    {
        try
        {
            PlaceWindow(680);
            _appWindow?.Show();
            Activate();
            var hwnd = WindowNative.GetWindowHandle(this);
            ShowWindow(hwnd, 9); // SW_RESTORE
            SetForegroundWindow(hwnd);
            QueryBox.Focus(FocusState.Programmatic);
        }
        catch
        {
            Activate();
        }
    }

    public void HideLauncher()
    {
        try
        {
            // 隐藏而不是退出进程（正式版由热键再次唤起）
            _appWindow?.Hide();
        }
        catch
        {
            // 兜底：最小化
            try
            {
                if (_appWindow?.Presenter is OverlappedPresenter p)
                    p.Minimize();
            }
            catch { }
        }
    }

    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr hWnd);

    private void OnList(object sender, RoutedEventArgs e)
    {
        _view = "list";
        ResultList.Visibility = Visibility.Visible;
        ResultGrid.Visibility = Visibility.Collapsed;
    }

    private void OnGrid(object sender, RoutedEventArgs e)
    {
        _view = "grid";
        ResultList.Visibility = Visibility.Collapsed;
        ResultGrid.Visibility = Visibility.Visible;
    }

    private void OnOpenSettings(object sender, RoutedEventArgs e)
    {
        // 进设置时暂时不要因失焦关掉（点 Toggle 等会抢焦点）
        _hideOnDeactivate = false;
        SettingsPanel.Visibility = Visibility.Visible;
    }

    private void OnBack(object sender, RoutedEventArgs e)
    {
        SettingsPanel.Visibility = Visibility.Collapsed;
        _hideOnDeactivate = true;
        QueryBox.Focus(FocusState.Programmatic);
    }

    private void OnQueryChanged(object sender, TextChangedEventArgs e)
    {
        _debounce?.Stop();
        _debounce = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(50) };
        _debounce.Tick += async (_, _) =>
        {
            _debounce.Stop();
            _active = 0;
            await SearchAsync(QueryBox.Text ?? "");
        };
        _debounce.Start();
    }

    private async void OnQueryKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter)
        {
            e.Handled = true;
            await InvokeAsync();
            return;
        }
        if (e.Key == VirtualKey.Escape)
        {
            e.Handled = true;
            if (SettingsPanel.Visibility == Visibility.Visible)
            {
                OnBack(sender, e);
                return;
            }
            if (!string.IsNullOrEmpty(QueryBox.Text))
            {
                QueryBox.Text = "";
                await SearchAsync("");
            }
            else
            {
                // 空输入再 Esc → 隐藏（uTools 行为）
                HideLauncher();
            }
            return;
        }
        if (e.Key == VirtualKey.Down && _items.Count > 0)
        {
            e.Handled = true;
            _active = Math.Min(_active + 1, _items.Count - 1);
            SyncSel();
        }
        else if (e.Key == VirtualKey.Up && _items.Count > 0)
        {
            e.Handled = true;
            _active = Math.Max(_active - 1, 0);
            SyncSel();
        }
    }

    private void SyncSel()
    {
        if (_items.Count == 0) return;
        _active = Math.Clamp(_active, 0, _items.Count - 1);
        if (_view == "list")
        {
            ResultList.SelectedIndex = _active;
            ResultList.ScrollIntoView(_items[_active]);
        }
        else
        {
            ResultGrid.SelectedIndex = _active;
            ResultGrid.ScrollIntoView(_items[_active]);
        }
        Footer.Text = _items[_active].Source + " · Esc 隐藏 · 失焦隐藏";
    }

    private async void OnItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is not CandidateDto c) return;
        _active = _items.IndexOf(c);
        SyncSel();
        await InvokeAsync();
    }

    private async Task SearchAsync(string text)
    {
        var result = DemoData.Query(text);
        _items.Clear();
        foreach (var i in result.Items) _items.Add(i);
        if (_items.Count > 0)
        {
            _active = 0;
            SyncSel();
        }
        else
        {
            Footer.Text = "未找到相关结果";
        }
        await Task.CompletedTask;
    }

    private async Task InvokeAsync()
    {
        if (_items.Count == 0 || _active < 0 || _active >= _items.Count) return;
        var item = _items[_active];
        if (item.Id == "sys.settings" || item.Title.Contains("设置"))
        {
            OnOpenSettings(this, new RoutedEventArgs());
            return;
        }
        Footer.Text = "已执行：" + item.Title;
        // 执行后隐藏（可配置，默认开）
        await Task.Delay(120);
        HideLauncher();
    }

    private void FillFavorites()
    {
        FavItems.Children.Clear();
        foreach (var id in new[] { "app.wt", "app.code", "app.chrome", "app.explorer" })
        {
            var c = DemoData.Find(id);
            if (c is null) continue;
            var name = c.Title.Length <= 8 ? c.Title : c.Title[..8];
            var title = c.Title;
            var btn = new Button { Content = name, Padding = new Thickness(10, 6, 10, 6) };
            btn.Click += async (_, _) =>
            {
                Footer.Text = "已执行：" + title;
                await Task.Delay(120);
                HideLauncher();
            };
            FavItems.Children.Add(btn);
        }
    }
}
