using System.Runtime.InteropServices;
using System.Threading;

namespace Spark.UI.Services;

/// <summary>
/// 监听 Host 托盘"退出"/host.exit 发出的命名事件 Local\SparkLauncherExit_v1，
/// 收到后整个应用退出（host 与 UI 是独立进程，UI 需要自己的退出信号）。
/// 不用 pipe 通知：host 的同步管道句柄在 read_loop 挂读时无法并行写（实测延迟 11-30s）。
/// </summary>
public sealed class ExitWatcher : IDisposable
{
    public const string EventName = "Local\\SparkLauncherExit_v1";

    private readonly Action _onExit;
    private readonly CancellationTokenSource _cts = new();
    private readonly Thread _thread;
    private IntPtr _event = IntPtr.Zero;

    public ExitWatcher(Action onExit)
    {
        _onExit = onExit;
        _thread = new Thread(Loop)
        {
            IsBackground = true,
            Name = "SparkExitWatcher"
        };
        _thread.Start();
    }

    private void Loop()
    {
        // bManualReset=false, bInitialState=false
        _event = CreateEventW(IntPtr.Zero, false, false, EventName);
        if (_event == IntPtr.Zero)
        {
            App.Log("ExitWatcher", new InvalidOperationException("CreateEvent failed"));
            return;
        }

        try
        {
            while (!_cts.IsCancellationRequested)
            {
                var wr = WaitForSingleObject(_event, 500);
                if (wr == 0) // WAIT_OBJECT_0
                {
                    try { _onExit(); }
                    catch (Exception ex) { App.Log("ExitCallback", ex); }
                }
            }
        }
        finally
        {
            if (_event != IntPtr.Zero)
            {
                CloseHandle(_event);
                _event = IntPtr.Zero;
            }
        }
    }

    public void Dispose()
    {
        _cts.Cancel();
        try { _thread.Join(1000); } catch { /* ignore */ }
        _cts.Dispose();
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateEventW(IntPtr lpEventAttributes, bool bManualReset,
        bool bInitialState, string lpName);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WaitForSingleObject(IntPtr hHandle, uint dwMilliseconds);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr hObject);
}
