using Microsoft.UI.Xaml;
using Microsoft.UI.Windowing;
using Microsoft.UI;
using WinRT.Interop;
using Spark.UI.Services;
using Spark.UI.Views;

namespace Spark.UI;

public sealed partial class MainWindow : Window
{
    public HostIpcClient Ipc { get; } = new();

    public MainWindow()
    {
        InitializeComponent();
        TryResize();
    }

    private void TryResize()
    {
        try
        {
            var hwnd = WindowNative.GetWindowHandle(this);
            var id = Win32Interop.GetWindowIdFromWindow(hwnd);
            var appWindow = AppWindow.GetFromWindowId(id);
            appWindow.Resize(new Windows.Graphics.SizeInt32(720, 560));
        }
        catch
        {
            // ignore
        }
    }

    public void NavigateToSettings()
    {
        SearchPage.Visibility = Visibility.Collapsed;
        SettingsPage.Visibility = Visibility.Visible;
    }

    public void NavigateToSearch()
    {
        SettingsPage.Visibility = Visibility.Collapsed;
        SearchPage.Visibility = Visibility.Visible;
        if (SearchPage is SearchView sv)
            sv.FocusQueryBox();
    }
}
