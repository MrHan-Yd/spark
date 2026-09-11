using Spark.UI.Models;
using Spark.UI.Views;

namespace Spark.UI.Services;

/// <summary>
/// 插件窗口注册表。默认单开——同一插件再次触发时聚焦已有窗口并推送新输入，
/// 而不是叠开第二个（《插件开发规范》§6）。清单 <c>window.multi_instance = true</c>
/// 时改为多开：每次触发都新开一个窗口，此时 native 纯应用插件的 exe 生命周期
/// 变为"最后一个窗口关闭才关停"，故本类同时是 exe 关停的唯一判据。
/// </summary>
public static class PluginWindowHost
{
    /// <summary>插件 id → 该插件当前打开的窗口（单开插件恒为 0/1 个，多开插件可多个）。
    /// 用列表而非单个窗口，让两种模式共用一套登记与关停逻辑。</summary>
    private static readonly Dictionary<string, List<PluginWindow>> _open = new();

    /// <summary>
    /// 打开或聚焦插件窗口。<paramref name="info"/> 由 host.plugin.open 返回。
    /// 必须在 UI 线程调用。devMode=true 且窗口已开时即时补开 DevTools（不关旧开新——
    /// 旧窗的关停通知晚到会误杀新页首次 rpc 懒启动的 exe）。每个插件窗口持有自己的
    /// IPC 连接，慢 rpc 不与主窗口搜索共用管道。
    /// </summary>
    public static void OpenOrFocus(PluginOpenInfoDto info,
        string input, string command, string rawQuery, bool devMode)
    {
        if (!info.Window.MultiInstance
            && _open.TryGetValue(info.Id, out var existing)
            && existing.Count > 0)
        {
            existing[0].FocusWith(input, command, rawQuery, devMode);
            return;
        }

        var win = new PluginWindow(info, input, command, rawQuery, devMode);
        if (!_open.TryGetValue(info.Id, out var list))
        {
            list = new List<PluginWindow>();
            _open[info.Id] = list;
        }
        list.Add(win);
        win.Closed += (_, _) => Remove(info.Id, win);
        win.Activate();
    }

    /// <summary>注销已关闭的窗口；该插件最后一个窗口关掉后移除整条记录。</summary>
    private static void Remove(string pluginId, PluginWindow win)
    {
        if (!_open.TryGetValue(pluginId, out var list)) return;
        list.Remove(win);
        if (list.Count == 0) _open.Remove(pluginId);
    }

    /// <summary>
    /// 该插件除 <paramref name="except"/> 外是否还有窗口开着。唯一用途：native 纯应用插件
    /// 的 exe 与页面同生命周期，只有最后一个窗口关闭才允许通知 host 关停 exe——
    /// 否则多开时关掉一个窗就把 exe 杀了，其余窗口的 spark.rpc 全断。
    /// 显式传入自身是因为 <c>Closed</c> 的注销回调与窗口自身处理器的执行顺序取决于
    /// 订阅顺序，不能依赖"此刻已注销"。
    /// </summary>
    public static bool HasOtherOpen(string pluginId, PluginWindow except)
        => _open.TryGetValue(pluginId, out var list)
           && list.Any(w => !ReferenceEquals(w, except));

    /// <summary>主程序退出前关掉所有插件窗口，避免残留顶层窗口。</summary>
    public static void CloseAll()
    {
        // 内外两层都必须先快照：win.Close() 会同步触发 Closed → Remove，就地枚举
        // 正在被修改的 List 会抛 InvalidOperationException（同 CloseIfOpen 的处理）。
        foreach (var list in _open.Values.ToList())
        {
            foreach (var win in list.ToList())
            {
                try { win.Close(); } catch (Exception ex) { App.Log("PluginWindowClose", ex); }
            }
        }
        _open.Clear();
    }

    /// <summary>插件被禁用/卸载后，它已开着的窗口必须一并关掉（否则页面还能继续调 spark.*）。
    /// 多开时关掉该插件的全部窗口。</summary>
    public static void CloseIfOpen(string pluginId)
    {
        if (!_open.TryGetValue(pluginId, out var list)) return;
        foreach (var win in list.ToList())
        {
            try { win.Close(); } catch (Exception ex) { App.Log("PluginWindowClose", ex); }
        }
        _open.Remove(pluginId);
    }
}
