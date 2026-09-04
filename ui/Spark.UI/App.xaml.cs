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
            // 单实例：已有 spark 在跑时，唤醒它（发 toggle 事件）并退出自己。
            // Host 热键路径只会在没有 spark 进程时才拉起，这里兜底防止手动重复启动。
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
            // 窗口显示时机由 MainWindow 自行控制（构造先隐藏，内容 Loaded 后再显示），
            // 这里不能 Activate——会立即显示窗口，破坏启动防闪框逻辑。
        }
        catch (Exception ex)
        {
            Log("OnLaunched", ex);
            try { MessageBoxW(IntPtr.Zero, ex.ToString(), "Spark UI 启动失败", 0x10); } catch { }
        }
    }

    /// <summary>诊断日志写锁：AppendAllText 并发写会互相截断丢行，串行化代价可忽略
    /// （调用点都在低频路径，热路径已清理）。</summary>
    private static readonly object LogGate = new();
    private const long LogRotateBytes = 5 * 1024 * 1024;

    private static void LogFile(string body)
    {
        try
        {
            var dir = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "Spark");
            var path = Path.Combine(dir, "ui-crash.log");
            lock (LogGate)
            {
                Directory.CreateDirectory(dir);
                // 轮转防无限增长：日志越大每次同步写越慢，也会悄悄吃磁盘。
                // 超限把当前文件挪成 .old，新行写入全新文件；.old 被占用等轮转失败时
                // 只影响这一次轮转，照常追加。
                try
                {
                    if (File.Exists(path) && new FileInfo(path).Length > LogRotateBytes)
                        File.Move(path, path + ".old", overwrite: true);
                }
                catch { }
                File.AppendAllText(path, body);
            }
        }
        catch { }
    }

    internal static void Log(string tag, Exception ex)
    {
        LogFile($"[{DateTime.Now:O}] {tag}\n{ex}\nHR=0x{ex.HResult:X8}\n\n");
    }

    internal static void Log(string tag, string msg)
    {
        LogFile($"[{DateTime.Now:O}] {tag}\n{msg}\n\n");
    }
}
