using System.Runtime.InteropServices;
using System.Threading;

namespace Spark.UI.Services;

/// <summary>
/// 监听 Host 热键发出的命名事件 Local\SparkLauncherToggle_v1。
/// 不依赖管道推送，Alt+Space 更稳。
/// </summary>
public sealed class ToggleWatcher : IDisposable
{
    public const string EventName = "Local\\SparkLauncherToggle_v1";

    private readonly Action _onToggle;
    private readonly CancellationTokenSource _cts = new();
    private readonly Thread _thread;
    private IntPtr _event = IntPtr.Zero;

    public ToggleWatcher(Action onToggle)
    {
        _onToggle = onToggle;
        _thread = new Thread(Loop)
        {
            IsBackground = true,
            Name = "SparkToggleWatcher"
        };
        _thread.Start();
    }

    private void Loop()
    {
        // bManualReset=false, bInitialState=false
        _event = CreateEventW(IntPtr.Zero, false, false, EventName);
        if (_event == IntPtr.Zero)
        {
            App.Log("ToggleWatcher", new InvalidOperationException("CreateEvent failed"));
            return;
        }

        try
        {
            while (!_cts.IsCancellationRequested)
            {
                var wr = WaitForSingleObject(_event, 500);
                if (wr == 0) // WAIT_OBJECT_0
                {
                    try { _onToggle(); }
                    catch (Exception ex) { App.Log("ToggleCallback", ex); }
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
