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

    // 弹出动画（对齐原型 pop-in / pop-out）
    private readonly CompositeTransform _pop = new();
    private Storyboard _animIn = new();
    private Storyboard _animOut = new();
    private bool _acrylicOk;
    /// <summary>同步设置控件时避免触发保存/换主题副作用。</summary>
    private bool _syncing;

    public MainWindow()
    {
        try { InitializeComponent(); }
        catch (Exception ex) { App.Log("InitializeComponent", ex); throw; }

        ExtendsContentIntoTitleBar = true;
        ResultList.ItemsSource = _items;
        ResultGrid.ItemsSource = _items;
        LocalState.Load();

        Root.RenderTransform = _pop;

        // 玻璃背景：先试 Acrylic，失败自动退回稳定深色（历史上有 Acrylic 闪退问题）
        try
        {
            SystemBackdrop = new DesktopAcrylicBackdrop();
            _acrylicOk = true;
        }
        catch (Exception ex) { App.Log("AcrylicBackdrop", ex); _acrylicOk = false; }

        _hideOnDeactivate = LocalState.Ui.HideOnFocusLost;
        ApplyTheme();
        SetView(LocalState.Ui.DefaultView == "grid");
        BuildAnimations();

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
            RenderFavorites();
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
    }

    // ==================== 弹出动画（原型 pop-in 0.28s / pop-out 0.16s） ====================

    private void BuildAnimations()
    {
        var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
        var inDur = new Duration(TimeSpan.FromMilliseconds(280));
        var outDur = new Duration(TimeSpan.FromMilliseconds(160));

        _animIn = BuildAnim(0, 1, 0.96, 1, 6, 0, inDur, ease);
        _animOut = BuildAnim(1, 0, 1, 0.97, 0, 4, outDur, ease);
        _animOut.Completed += (_, _) => HideNow();
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

        PlaceWindow(LocalState.Ui.WindowWidth, 590);
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
        _appWindow.Move(new PointInt32(
            work.X + (work.Width - w) / 2,
            work.Y + Math.Max(80, work.Height / 6)));
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

            ForceForeground();
            Activate();
            SettingsPanel.Visibility = Visibility.Collapsed;
            QueryBox.Text = "";
            _ = RefreshResultsAsync("");
            QueryBox.Focus(FocusState.Programmatic);

            // pop-in（对齐原型 .launcher 入场）
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
        _animIn.Stop();
        _animOut.Begin();
    }

    private void HideNow()
    {
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

        // 折叠状态
        var collapsed = !fav.Expanded;
        FavBody.Visibility = collapsed ? Visibility.Collapsed : Visibility.Visible;
        if (FavChevronPath.RenderTransform is RotateTransform rt)
            rt.Angle = collapsed ? -90 : 0;
        ToolTipService.SetToolTip(FavToggle, collapsed ? "展开收藏" : "收起收藏");
    }

    private void OnFavToggle(object sender, RoutedEventArgs e)
    {
        var fav = LocalState.Fav;
        fav.Expanded = !fav.Expanded;
        LocalState.SaveFav();
        RenderFavorites();
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
        }
    }

    // ==================== 视图切换 ====================

    private void SetView(bool grid)
    {
        _gridView = grid;
        ResultList.Visibility = grid ? Visibility.Collapsed : Visibility.Visible;
        ResultGrid.Visibility = grid ? Visibility.Visible : Visibility.Collapsed;
        var res = Root.Resources;
        var activeBg = (Brush)res["AccentSoftBrush"];
        var idleBg = new SolidColorBrush(Colors.Transparent);
        BtnViewList.Background = grid ? idleBg : activeBg;
        BtnViewGrid.Background = grid ? activeBg : idleBg;
        if (BtnViewList.Content is XamlPath pl)
            pl.Stroke = grid ? (Brush)res["TextTertiaryBrush"] : (Brush)res["TextPrimaryBrush"];
        if (BtnViewGrid.Content is XamlPath pg)
            pg.Fill = grid ? (Brush)res["TextPrimaryBrush"] : (Brush)res["TextTertiaryBrush"];
    }

    private void OnViewList(object sender, RoutedEventArgs e) => SetView(false);

    private void OnViewGrid(object sender, RoutedEventArgs e) => SetView(true);

    // ==================== 设置 ====================

    private void OnOpenSettings(object sender, RoutedEventArgs e)
    {
        SettingsPanel.Visibility = Visibility.Visible;
        AboutText.Text = _host.IsConnected
            ? "Spark UI · 已连接 Host"
            : "Spark UI · 未连接 Host（演示）";
        SyncSettingsUi();
        ShowPane("general");
    }

    private void OnCloseSettings(object sender, RoutedEventArgs e)
    {
        SettingsPanel.Visibility = Visibility.Collapsed;
        QueryBox.Focus(FocusState.Programmatic);
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
        _animIn.Stop();
        _animOut.Stop();
        ResetPop();
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
}
