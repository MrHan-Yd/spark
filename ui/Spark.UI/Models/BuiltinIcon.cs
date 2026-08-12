using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace Spark.UI.Models;

/// <summary>内置命令的图标样式（字形 + 专属颜色 + 字体）。
/// 搜索结果行与设置页"命令"栏共用；有系统图标的命令（回收站等）由
/// AppIconService 提取真实图标覆盖字形显示。</summary>
public static class BuiltinIcon
{
    private static readonly Dictionary<string, (string Glyph, Color Color)> Glyphs = new()
    {
        ["builtin.lock"] = ("\uE72E", Color.FromArgb(255, 10, 132, 255)),              // Lock
        ["builtin.shutdown"] = ("\uE7E8", Color.FromArgb(255, 255, 69, 58)),           // PowerButton
        ["builtin.reboot"] = ("\uE777", Color.FromArgb(255, 255, 159, 10)),            // Refresh
        ["builtin.logoff"] = ("\uE77B", Color.FromArgb(255, 175, 82, 222)),            // SignOut
        ["builtin.sleep"] = ("\uE708", Color.FromArgb(255, 94, 92, 230)),              // Moon
        ["builtin.empty_recycle_bin"] = ("\uE74D", Color.FromArgb(255, 255, 55, 95)),  // Delete
        // 回收站优先显示系统纸篓图标（AppIconService 特判）；此处仅在提取失败时兜底
        ["builtin.recycle_bin"] = ("\uE74D", Color.FromArgb(255, 170, 140, 100)),      // Delete
        ["builtin.screenshot"] = ("\uE722", Color.FromArgb(255, 48, 176, 199)),        // Camera
        ["builtin.settings"] = ("\uE713", Color.FromArgb(255, 142, 142, 147)),         // Settings
        ["builtin.explorer"] = ("\uE8B7", Color.FromArgb(255, 52, 199, 89)),           // FolderOpen
        ["builtin.remote_desktop"] = ("\uE8CE", Color.FromArgb(255, 64, 156, 255)),    // Remote
        ["builtin.lan_ip"] = ("\uE968", Color.FromArgb(255, 0, 199, 190)),             // Network
        ["builtin.public_ip"] = ("\uE774", Color.FromArgb(255, 88, 101, 242)),         // Globe
        // 商店应用兜底字形（正常情况由 AppIconService 提取包内 AppList logo 覆盖）
        ["builtin.calc"] = ("\uE1D0", Color.FromArgb(255, 52, 120, 246)),              // Calculator
        ["builtin.paint"] = ("\uE790", Color.FromArgb(255, 255, 111, 97)),             // 调色板
        // ===== 管理工具 / 控制面板（字形绘制，无系统图标） =====
        ["builtin.sysprops"] = ("\uE7B8", Color.FromArgb(255, 96, 125, 139)),          // PC
        ["builtin.env_vars"] = ("\uE8AC", Color.FromArgb(255, 38, 166, 154)),          // 变量（A 框）
        ["builtin.device_manager"] = ("\uE950", Color.FromArgb(255, 124, 179, 66)),    // 芯片
        ["builtin.disk_management"] = ("\uE74E", Color.FromArgb(255, 255, 138, 101)),  // 软盘
        ["builtin.computer_management"] = ("\uE960", Color.FromArgb(255, 13, 71, 161)),// 显示器
        ["builtin.services"] = ("\uE821", Color.FromArgb(255, 255, 167, 38)),          // 工具箱
        ["builtin.event_viewer"] = ("\uE7C3", Color.FromArgb(255, 129, 212, 250)),     // 日志文档
        ["builtin.task_scheduler"] = ("\uE823", Color.FromArgb(255, 63, 81, 181)),     // 时钟
        ["builtin.performance_monitor"] = ("\uE9D9", Color.FromArgb(255, 46, 125, 50)),// 脉搏线
        ["builtin.secpol"] = ("\uE72E", Color.FromArgb(255, 255, 193, 7)),             // Lock
        ["builtin.gpedit"] = ("\uE713", Color.FromArgb(255, 97, 97, 97)),              // Settings
        ["builtin.shared_folders"] = ("\uE72D", Color.FromArgb(255, 240, 98, 146)),    // Share
        ["builtin.users_groups"] = ("\uE716", Color.FromArgb(255, 69, 39, 160)),       // People
        ["builtin.programs_features"] = ("\uE71D", Color.FromArgb(255, 139, 195, 74)), // 清单
        ["builtin.network_connections"] = ("\uE701", Color.FromArgb(255, 2, 119, 189)),// Wifi
        ["builtin.sound"] = ("\uE767", Color.FromArgb(255, 194, 24, 91)),              // 音箱
        ["builtin.power_options"] = ("\uE945", Color.FromArgb(255, 255, 87, 34)),      // 闪电
        ["builtin.date_time"] = ("\uE787", Color.FromArgb(255, 0, 121, 107)),          // Calendar
        ["builtin.mouse"] = ("\uE962", Color.FromArgb(255, 120, 120, 120)),            // 鼠标
        ["builtin.region"] = ("\uE774", Color.FromArgb(255, 0, 150, 136)),             // Globe
        ["builtin.fonts"] = ("\uE8D2", Color.FromArgb(255, 156, 39, 176)),             // Font（Aa）
    };

    public static readonly FontFamily FontFluent = new("Segoe Fluent Icons");
    public static readonly FontFamily FontDefault = new("Segoe UI");

    public static (string Glyph, Color Color)? For(string id)
        => Glyphs.TryGetValue(id, out var v) ? v : null;

    /// <summary>字形：有专属图标用 Fluent 字形，否则首字母。</summary>
    public static string GlyphFor(string id, string title)
    {
        if (For(id) is { } bi)
        {
            return bi.Glyph;
        }
        if (!string.IsNullOrEmpty(title))
        {
            var ch = title.Trim()[0];
            return char.IsLetterOrDigit(ch) ? ch.ToString().ToUpperInvariant() : "•";
        }
        return "?";
    }

    public static FontFamily FontFor(string id) => For(id) is null ? FontDefault : FontFluent;

    /// <summary>专属字形更大更醒目（19px vs 13px）。</summary>
    public static double FontSizeFor(string id) => For(id) is null ? 13 : 19;

    public static Color ColorFor(string id)
        => For(id) is { } bi ? bi.Color : Color.FromArgb(255, 10, 132, 255);
}
