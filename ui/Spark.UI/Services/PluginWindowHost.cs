using Spark.UI.Models;
using Spark.UI.Views;

namespace Spark.UI.Services;

/// <summary>
/// 插件窗口注册表：默认单开——同一插件再次触发时聚焦已有窗口并推送新输入，
/// 而不是叠开第二个（《插件开发规范》§6）。
/// </summary>
public static class PluginWindowHost
{
    private static readonly Dictionary<string, PluginWindow> _open = new();

    /// <summary>
    /// 打开或聚焦插件窗口。<paramref name="info"/> 由 host.plugin.open 返回。
    /// 必须在 UI 线程调用。devMode=true 且窗口已开时即时补开 DevTools（不关旧开新——
    /// 旧窗的关停通知晚到会误杀新页首次 rpc 懒启动的 exe）。每个插件窗口持有自己的
    /// IPC 连接，慢 rpc 不与主窗口搜索共用管道。
    /// </summary>
    public static void OpenOrFocus(PluginOpenInfoDto info,
        string input, string command, string rawQuery, bool devMode)
    {
        if (_open.TryGetValue(info.Id, out var existing))
        {
            existing.FocusWith(input, command, rawQuery, devMode);
            return;
        }

        var win = new PluginWindow(info, input, command, rawQuery, devMode);
        _open[info.Id] = win;
        win.Closed += (_, _) => _open.Remove(info.Id);
        win.Activate();
    }

    /// <summary>主程序退出前关掉所有插件窗口，避免残留顶层窗口。</summary>
    public static void CloseAll()
    {
        foreach (var win in _open.Values.ToList())
        {
            try { win.Close(); } catch (Exception ex) { App.Log("PluginWindowClose", ex); }
        }
        _open.Clear();
    }

    /// <summary>插件被禁用/卸载后，它已开着的窗口必须一并关掉（否则页面还能继续调 spark.*）。</summary>
    public static void CloseIfOpen(string pluginId)
    {
        if (!_open.TryGetValue(pluginId, out var win)) return;
        try { win.Close(); } catch (Exception ex) { App.Log("PluginWindowClose", ex); }
        _open.Remove(pluginId);
    }
}
