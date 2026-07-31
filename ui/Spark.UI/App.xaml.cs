using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using Spark.UI.Services;

namespace Spark.UI;

public partial class App : Application
{
    private Window? _window;
    /// <summary>单实例锁；持有到进程退出，防止被 GC。</summary>
    private Mutex? _singleInstance;

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
            // 单实例：已有 spark-ui 在跑时，唤醒它（发 toggle 事件）并退出自己。
            // Host 热键路径只会在没有 spark-ui 进程时才拉起，这里兜底防止手动重复启动。
            var m = new Mutex(false, "Local\\SparkUISingleInstance_v1", out var createdNew);
            bool owned;
            try { owned = m.WaitOne(0); }
            catch (AbandonedMutexException) { owned = true; } // 前任异常退出，锁被放弃，我们接管
            if (!owned)
            {
                try { ToggleWatcher.Signal(); } catch { /* ignore */ }
                m.Dispose();
                Exit(); // 无窗口进程必须显式退出，否则会残留
                return;
            }
            _singleInstance = m; // 持有锁直到进程退出，防止被 GC

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
