using System.ComponentModel;
using System.Runtime.CompilerServices;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Spark.UI.Models;
using Spark.UI.Services;
using Windows.UI;

namespace Spark.UI.ViewModels;

/// <summary>
/// 设置-插件页的一行。把 <see cref="PluginInfoDto"/> 摊平成 XAML 直接可绑的形状，
/// 并承载"启用开关 / 权限授权"两处交互状态。
/// </summary>
public sealed class PluginRowVm : INotifyPropertyChanged
{
    private readonly PluginInfoDto _info;

    public PluginRowVm(PluginInfoDto info)
    {
        _info = info;
        _enabled = info.Enabled;
        SyncedEnabled = info.Enabled;
        SignState = ParseSignState(info.SignState);
        Permissions = info.Permissions
            .Select(p => new PermissionVm(p, info.Granted.Contains(p)))
            .ToList();
        if (!string.IsNullOrEmpty(info.Icon) && File.Exists(info.Icon))
        {
            try { IconImage = new BitmapImage(new Uri(info.Icon)); }
            catch (Exception ex) { App.Log("PluginIcon", ex); }
        }
    }

    public string Id => _info.Id;
    public string Name => _info.Name;
    public string Version => "v" + _info.Version;
    public string Description => _info.Description ?? "";
    public bool IsDev => string.Equals(_info.Source, "dev", StringComparison.OrdinalIgnoreCase);

    /// <summary>解析后的签名状态；host 老版本未返回时回落 Unsigned。</summary>
    public PluginSignState SignState { get; }

    /// <summary>签名角标文案；Unsigned 为空（不渲染角标）。</summary>
    public string SignBadgeText => SignState switch
    {
        PluginSignState.Official => "官方",
        PluginSignState.ThirdParty => "已签名",
        PluginSignState.Invalid => "签名失效",
        _ => "",
    };

    /// <summary>Official/ThirdParty/Invalid 显示角标；Unsigned 隐藏。</summary>
    public Visibility SignBadgeVisibility =>
        SignState is PluginSignState.Official
            or PluginSignState.ThirdParty
            or PluginSignState.Invalid
            ? Visibility.Visible
            : Visibility.Collapsed;

    /// <summary>角标背景色；与 <see cref="SignBadgeText"/> 配对。画刷固定，缓存为单例避免列表滚动/容器回收时重复分配。</summary>
    public Brush SignBadgeBackground => SignState switch
    {
        PluginSignState.Official => OfficialBadgeBg,
        PluginSignState.ThirdParty => ThirdPartyBadgeBg,
        PluginSignState.Invalid => InvalidBadgeBg,
        _ => TransparentBrush,
    };

    /// <summary>角标前景色。</summary>
    public Brush SignBadgeForeground => SignState switch
    {
        PluginSignState.Official => OfficialBadgeFg,
        PluginSignState.ThirdParty => ThirdPartyBadgeFg,
        PluginSignState.Invalid => InvalidBadgeFg,
        _ => TransparentBrush,
    };

    // 签名角标配色（与"开发"角标同款半透明填充+不透明前景）。
    private static readonly Brush OfficialBadgeBg = new SolidColorBrush(Color.FromArgb(0x33, 0x30, 0xD1, 0x58));
    private static readonly Brush OfficialBadgeFg = new SolidColorBrush(Color.FromArgb(0xFF, 0x30, 0xD1, 0x58));
    private static readonly Brush ThirdPartyBadgeBg = new SolidColorBrush(Color.FromArgb(0x33, 0x4C, 0x9A, 0xFF));
    private static readonly Brush ThirdPartyBadgeFg = new SolidColorBrush(Color.FromArgb(0xFF, 0x4C, 0x9A, 0xFF));
    private static readonly Brush InvalidBadgeBg = new SolidColorBrush(Color.FromArgb(0x33, 0xFF, 0x45, 0x3A));
    private static readonly Brush InvalidBadgeFg = new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0x45, 0x3A));
    private static readonly Brush TransparentBrush = new SolidColorBrush(Colors.Transparent);

    /// <summary>签名失效红色提示行；仅 Invalid 可见。</summary>
    public Visibility InvalidHintVisibility =>
        SignState == PluginSignState.Invalid ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>触发关键字 chips，如 ["hi"]；无 keyword feature 时为空。</summary>
    public List<string> Keywords => _info.Features
        .Where(f => !string.IsNullOrEmpty(f.Keyword))
        .Select(f => f.Keyword!)
        .Distinct()
        .ToList();

    public List<PermissionVm> Permissions { get; }

    public ImageSource? IconImage { get; }

    public Visibility IconImageVisibility =>
        IconImage is null ? Visibility.Collapsed : Visibility.Visible;

    public Visibility IconTextVisibility =>
        IconImage is null ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>无图标时的字形兜底：取名字首字。</summary>
    public string IconLetter => Name.Length > 0 ? Name[..1].ToUpperInvariant() : "?";

    public Visibility DevBadgeVisibility => IsDev ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>调试按钮可见性：仅当通用设置开启开发者模式时显示。
    /// 每次访问都读 LocalState 的实时值；开关在通用页切换，回到插件页导航时列表整体重建，
    /// 因此无需 INotify，重建即重新求值。</summary>
    public Visibility DebugVisibility =>
        LocalState.Ui.DeveloperMode ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>开发目录加载的插件不能"卸载"（文件不归 Spark 管），只能禁用。</summary>
    public Visibility UninstallVisibility => IsDev ? Visibility.Collapsed : Visibility.Visible;

    public Visibility PermissionsVisibility =>
        Permissions.Count > 0 ? Visibility.Visible : Visibility.Collapsed;

    private bool _enabled;
    public bool Enabled
    {
        get => _enabled;
        set
        {
            if (_enabled == value) return;
            _enabled = value;
            OnPropertyChanged();
        }
    }

    /// <summary>
    /// host 侧的已知状态。ListView 容器是虚拟化按需生成的，绑定赋值会在
    /// 赋 ItemsSource 之后才触发 Toggled，用"同步中"布尔门闸挡不住；
    /// 改为比对此值——与它相同就是绑定回声，不回写 host。
    /// </summary>
    public bool SyncedEnabled { get; set; }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));

    /// <summary>把 host 的 snake_case 字符串解析为枚举；未知值回落 Unsigned。</summary>
    private static PluginSignState ParseSignState(string raw)
        => raw?.ToLowerInvariant() switch
        {
            "official" => PluginSignState.Official,
            "third_party" => PluginSignState.ThirdParty,
            "unsigned" => PluginSignState.Unsigned,
            "invalid" => PluginSignState.Invalid,
            _ => PluginSignState.Unsigned,
        };
}

/// <summary>一条权限的授权状态；勾选变化由插件页回写 host.plugin.grant。</summary>
public sealed class PermissionVm : INotifyPropertyChanged
{
    public PermissionVm(string key, bool granted)
    {
        Key = key;
        _granted = granted;
        SyncedGranted = granted;
    }

    public string Key { get; }

    /// <summary>host 侧已知的授权状态；与 <see cref="Granted"/> 相同即为绑定回声。</summary>
    public bool SyncedGranted { get; set; }

    /// <summary>中文显示名（对齐《插件开发规范》§7 权限表）。</summary>
    public string Display => Key switch
    {
        "clipboard" => "剪贴板",
        "notify" => "通知",
        "net" => "网络",
        "shell.open" => "打开外部程序",
        "fs.read" => "读文件",
        "fs.write" => "写文件",
        "window.alwaysOnTop" => "窗口置顶",
        "db" => "私有存储",
        _ => Key
    };

    private bool _granted;
    public bool Granted
    {
        get => _granted;
        set
        {
            if (_granted == value) return;
            _granted = value;
            OnPropertyChanged();
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
