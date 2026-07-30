using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;

namespace Spark.UI;

public partial class App : Application
{
    private Window? _window;

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int MessageBoxW(IntPtr hWnd, string text, string caption, uint type);

    public App()
    {
        UnhandledException += (_, e) =>
        {
            e.Handled = true;
            Log("Unhandled", e.Exception);
            try { MessageBoxW(IntPtr.Zero, e.Message + "\n\n" + e.Exception, "Spark UI", 0x10); } catch { }
        };
        this.InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        try
        {
            _window = new MainWindow();
            _window.Activate();
        }
        catch (Exception ex)
        {
            Log("OnLaunched", ex);
            try { MessageBoxW(IntPtr.Zero, ex.ToString(), "Spark UI 启动失败", 0x10); } catch { }
        }
    }

    internal static void Log(string tag, Exception ex)
    {
        try
        {
            var path = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "Spark", "ui-crash.log");
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.AppendAllText(path,
                $"[{DateTime.Now:O}] {tag}\n{ex}\nHR=0x{ex.HResult:X8}\n\n");
        }
        catch { }
    }
}
