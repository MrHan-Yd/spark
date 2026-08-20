using System.ComponentModel;
using System.Runtime.CompilerServices;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Spark.UI.Models;

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
