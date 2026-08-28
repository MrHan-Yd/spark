using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Text.Json.Serialization;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace Spark.UI.Models;

/// <summary>
/// 插件市场仓库索引根对象 (registry.json)
/// </summary>
public sealed class RegistryIndexDto
{
    [JsonPropertyName("schema")]
    public int Schema { get; set; } = 1;

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    [JsonPropertyName("zipball_url")]
    public string ZipballUrl { get; set; } = "";

    [JsonPropertyName("updated")]
    public string? Updated { get; set; }

    [JsonPropertyName("plugins")]
    public List<RegistryPluginDto> Plugins { get; set; } = new();
}

/// <summary>
/// 插件市场中单个插件信息
/// </summary>
public sealed class RegistryPluginDto
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    [JsonPropertyName("description")]
    public string Description { get; set; } = "";

    [JsonPropertyName("author")]
    public string Author { get; set; } = "";

    [JsonPropertyName("homepage")]
    public string? Homepage { get; set; }

    [JsonPropertyName("icon")]
    public string? Icon { get; set; }

    [JsonPropertyName("runtime")]
    public string Runtime { get; set; } = "webview";

    [JsonPropertyName("permissions")]
    public List<string> Permissions { get; set; } = new();

    [JsonPropertyName("latest")]
    public string Latest { get; set; } = "";

    [JsonPropertyName("versions")]
    public List<RegistryVersionDto> Versions { get; set; } = new();
}

/// <summary>
/// 插件版本明细
/// </summary>
public sealed class RegistryVersionDto
{
    [JsonPropertyName("version")]
    public string Version { get; set; } = "";

    [JsonPropertyName("path")]
    public string? Path { get; set; }

    [JsonPropertyName("url")]
    public string? Url { get; set; }

    [JsonPropertyName("sha256")]
    public string? Sha256 { get; set; }

    [JsonPropertyName("size")]
    public long? Size { get; set; }

    [JsonPropertyName("released")]
    public string? Released { get; set; }

    /// <summary>索引自带的官方签名摘要（规范 §3.3，可选）：仅用于市场卡片"官方"预判与
    /// 装前一致性预检。包内 signature.json 才是权威（规范 §4.6）——索引字段不含文件清单，
    /// 无法独立完成真验签，且三方仓库可任意伪造，展示侧必须配合来源门控（见 RegistryPluginViewDto）。</summary>
    [JsonPropertyName("signature")]
    public RegistrySignatureDto? Signature { get; set; }
}

/// <summary>registry.json <c>versions[].signature</c>（规范 §3.3）：与包内 signature.json
/// 同源的官方签名摘要（key_id + 签名值，不含文件清单）。</summary>
public sealed class RegistrySignatureDto
{
    /// <summary>host 内置官方密钥标识（crates/plugin-manager/src/signing/keys.rs）。
    /// 卡片"官方"预判的 key_id 白名单只有这一项；设置页三方公钥导入的冲突检查同用此值。</summary>
    public const string OfficialKeyId = "spark-official-v1";

    [JsonPropertyName("schema")]
    public int Schema { get; set; }

    [JsonPropertyName("key_id")]
    public string KeyId { get; set; } = "";

    [JsonPropertyName("algorithm")]
    public string Algorithm { get; set; } = "";

    [JsonPropertyName("signature")]
    public string Signature { get; set; } = "";
}

/// <summary>
/// 插件市场在 UI 展示与交互用的视图模型 DTO
/// </summary>
public sealed class RegistryPluginViewDto : INotifyPropertyChanged
{
    public RegistryPluginDto Plugin { get; set; } = new();
    public RegistryVersionDto TargetVersion { get; set; } = new();

    public string Id => Plugin.Id;
    public string Name => Plugin.Name;
    public string Description => Plugin.Description;
    public string Author => Plugin.Author;
    public string Runtime => Plugin.Runtime;
    public string VersionText => TargetVersion.Version;
    public bool IsNative => string.Equals(Plugin.Runtime, "native", StringComparison.OrdinalIgnoreCase);

    public string? InstalledVersion { get; set; }
    public bool IsInstalled => !string.IsNullOrEmpty(InstalledVersion);
    public bool CanUpdate { get; set; }
    public bool CanDowngrade { get; set; }

    /// <summary>数据源是否为内置官方仓库（LoadMarketplaceAsync 按仓库 URL 判定）。
    /// 三方仓库条目自带的 signature 字段可任意伪造，不参与"官方"预判——否则三方仓库
    /// 抄一个 key_id 就能整页挂官方角标钓鱼。三方源卡片只有在本地验签通过后才显角标。</summary>
    public bool IsOfficialSource { get; set; }

    /// <summary>已安装副本的验签状态原文（host plugin.list 的 snake_case sign_state），
    /// 由 LoadMarketplaceAsync 回填。已装版本与目标一致时优先于索引预判（包内权威，规范 §4.6）。</summary>
    public string? InstalledSignStateRaw { get; set; }

    /// <summary>已装版本与卡片目标版本一致（此时本地验签结果才可代表该版本内容）。</summary>
    private bool SameVersionAsTarget => IsInstalled && !CanUpdate && !CanDowngrade;

    /// <summary>
    /// 市场卡片签名状态（规范 Phase 4.3/4.4），取值优先级：
    /// 1. 已装版本==目标版本且本地验签非 Unsigned → 以本地为准（包内权威，含"签名失效"红徽）；
    /// 2. 官方源且索引 signature（schema=1/ed25519/key_id 命中官方）→ 预判"官方"，
    ///    下载前即可展示；安装时仍以 host 全量验签为准，索引自报永远不改变实际验签结果；
    /// 3. 其余（三方源未装/官方源字段异常）→ null，不显签名角标；
    ///    官方源但索引未带 signature 的版本由 <see cref="PendingSignBadge"/> 显示灰"待签名"。
    /// </summary>
    public PluginSignState? DisplaySignState
    {
        get
        {
            if (SameVersionAsTarget && InstalledSignStateRaw is not null)
            {
                var local = PluginSignStateParser.Parse(InstalledSignStateRaw);
                if (local != PluginSignState.Unsigned) return local;
            }
            if (!IsOfficialSource) return null;
            var sig = TargetVersion.Signature;
            return sig is not null
                && sig.Schema == 1
                && string.Equals(sig.Algorithm, "ed25519", StringComparison.OrdinalIgnoreCase)
                && string.Equals(sig.KeyId, RegistrySignatureDto.OfficialKeyId, StringComparison.Ordinal)
                ? PluginSignState.Official
                : null;
        }
    }

    /// <summary>官方源但索引未带 signature（3.0 过渡期，规范 Phase 4.3）：灰色"待签名"角标，仍可安装。</summary>
    public bool PendingSignBadge =>
        IsOfficialSource && DisplaySignState is null && TargetVersion.Signature is null;

    public Visibility SignBadgeVisibility =>
        DisplaySignState is not null || PendingSignBadge
            ? Visibility.Visible
            : Visibility.Collapsed;

    /// <summary>签名角标文案（与已装列表 PluginRowVm 同款措辞）。</summary>
    public string SignBadgeText => PendingSignBadge
        ? "待签名"
        : DisplaySignState switch
        {
            PluginSignState.Official => "官方",
            PluginSignState.ThirdParty => "已签名",
            PluginSignState.Invalid => "签名失效",
            _ => "",
        };

    // 角标配色与 PluginRowVm 保持一致（半透明填充 + 不透明前景），缓存为单例避免列表重建反复分配。
    private static readonly Brush OfficialBadgeBg = new SolidColorBrush(Color.FromArgb(0x33, 0x30, 0xD1, 0x58));
    private static readonly Brush OfficialBadgeFg = new SolidColorBrush(Color.FromArgb(0xFF, 0x30, 0xD1, 0x58));
    private static readonly Brush ThirdPartyBadgeBg = new SolidColorBrush(Color.FromArgb(0x33, 0x4C, 0x9A, 0xFF));
    private static readonly Brush ThirdPartyBadgeFg = new SolidColorBrush(Color.FromArgb(0xFF, 0x4C, 0x9A, 0xFF));
    private static readonly Brush InvalidBadgeBg = new SolidColorBrush(Color.FromArgb(0x33, 0xFF, 0x45, 0x3A));
    private static readonly Brush InvalidBadgeFg = new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0x45, 0x3A));
    private static readonly Brush PendingBadgeBg = new SolidColorBrush(Color.FromArgb(0x33, 0x80, 0x80, 0x80));
    private static readonly Brush PendingBadgeFg = new SolidColorBrush(Color.FromArgb(0xFF, 0xA6, 0xA6, 0xA6));
    private static readonly Brush TransparentBrush = new SolidColorBrush(Colors.Transparent);

    public Brush SignBadgeBackground => PendingSignBadge
        ? PendingBadgeBg
        : DisplaySignState switch
        {
            PluginSignState.Official => OfficialBadgeBg,
            PluginSignState.ThirdParty => ThirdPartyBadgeBg,
            PluginSignState.Invalid => InvalidBadgeBg,
            _ => TransparentBrush,
        };

    public Brush SignBadgeForeground => PendingSignBadge
        ? PendingBadgeFg
        : DisplaySignState switch
        {
            PluginSignState.Official => OfficialBadgeFg,
            PluginSignState.ThirdParty => ThirdPartyBadgeFg,
            PluginSignState.Invalid => InvalidBadgeFg,
            _ => TransparentBrush,
        };

    public string ActionButtonText
    {
        get
        {
            // 安装中按钮即进度载体：文案随水位走（下载 N% / 安装中…），波浪在按钮内从下往上填充
            if (IsInstalling) return ProgressText;
            if (!IsInstalled) return "安装";
            if (CanUpdate) return "更新";
            return "已是最新";
        }
    }

    /// <summary>安装中保持可点态（禁用态会压暗水体波浪）；防重复点击由
    /// OnInstallFromMarketplace 入口的 IsInstalling 守卫负责。</summary>
    public bool ActionButtonEnabled => !IsInstalled || CanUpdate;

    private bool _isInstalling;
    /// <summary>安装进行中：按钮内水体波浪 + 文案由下载回调实时驱动（INPC，无需重建列表）。</summary>
    public bool IsInstalling
    {
        get => _isInstalling;
        set
        {
            if (_isInstalling == value) return;
            _isInstalling = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(ActionButtonText));
            OnPropertyChanged(nameof(ActionButtonEnabled));
        }
    }

    private double _installProgress;
    /// <summary>下载进度 0-100；驱动按钮内水位与百分比文案。下载完成后保持满格进入安装阶段。</summary>
    public double InstallProgress
    {
        get => _installProgress;
        set
        {
            if (_installProgress == value) return;
            _installProgress = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(ActionButtonText));
        }
    }

    private bool _progressIndeterminate;
    /// <summary>总量未知（响应无 Content-Length）时文案走"下载中…"，水位停在流动的中间态。</summary>
    public bool ProgressIndeterminate
    {
        get => _progressIndeterminate;
        private set
        {
            if (_progressIndeterminate == value) return;
            _progressIndeterminate = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(ActionButtonText));
        }
    }

    /// <summary>安装中的按钮文案：下载中显示百分比；满格后是解压/安装阶段。</summary>
    public string ProgressText
    {
        get
        {
            if (ProgressIndeterminate) return "下载中…";
            return InstallProgress >= 100 ? "安装中…" : $"下载 {InstallProgress:0}%";
        }
    }

    /// <summary>下载回调（HttpClient 线程池线程，调用方已用 DispatcherQueue marshal 到 UI 线程）。
    /// total 非正数视为总量未知。</summary>
    public void ReportDownloadProgress(long received, long? total)
    {
        if (!IsInstalling) return;
        if (total is > 0)
        {
            ProgressIndeterminate = false;
            InstallProgress = Math.Min(100.0, received * 100.0 / total.Value);
        }
        else
        {
            ProgressIndeterminate = true;
        }
    }

    /// <summary>下载完成：水位置满（用户语义：满=下载完），文案切"安装中…"直至安装结果出来。</summary>
    public void ReportDownloadDone()
    {
        ProgressIndeterminate = false;
        InstallProgress = 100;
        OnPropertyChanged(nameof(ActionButtonText));
    }

    /// <summary>安装/更新完成后原位回填已装状态并通知绑定刷新（替代整表重建，避免列表闪烁、
    /// 波浪退场动画不中断）。调用方保证 version 即卡片目标版本（已装==目标）。</summary>
    public void UpdateInstalledState(string version, string? signStateRaw)
    {
        InstalledVersion = version;
        CanUpdate = false;
        CanDowngrade = false;
        InstalledSignStateRaw = signStateRaw;
        RefreshInstallBindings();
    }

    /// <summary>重算并通知所有依赖已装状态的绑定（按钮文案/可用性、签名角标）。</summary>
    public void RefreshInstallBindings()
    {
        OnPropertyChanged(nameof(ActionButtonText));
        OnPropertyChanged(nameof(ActionButtonEnabled));
        OnPropertyChanged(nameof(SignBadgeVisibility));
        OnPropertyChanged(nameof(SignBadgeText));
        OnPropertyChanged(nameof(SignBadgeBackground));
        OnPropertyChanged(nameof(SignBadgeForeground));
    }

    public Visibility NativeBadgeVisibility =>
        IsNative ? Visibility.Visible : Visibility.Collapsed;

    public string PermissionsSummary =>
        Plugin.Permissions.Count > 0 ? string.Join(", ", Plugin.Permissions) : "无特殊权限";

    /// <summary>无图标时的字形兜底：取名字首字（与已装插件列表同款）。</summary>
    public string IconLetter => Name.Length > 0 ? Name[..1].ToUpperInvariant() : "?";

    private ImageSource? _iconImage;

    /// <summary>远程加载的市场图标；LoadMarketIconAsync 下载解码后补上并通知刷新，
    /// 失败保持 null → 字母占位。列表刷新会整表重建 DTO，迟到结果落孤立对象无害。</summary>
    public ImageSource? IconImage
    {
        get => _iconImage;
        set
        {
            _iconImage = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(IconImageVisibility));
            OnPropertyChanged(nameof(IconLetterVisibility));
        }
    }

    public Visibility IconImageVisibility =>
        IconImage is null ? Visibility.Collapsed : Visibility.Visible;

    public Visibility IconLetterVisibility =>
        IconImage is null ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>作者缺失时隐藏作者元信息块。</summary>
    public Visibility AuthorVisibility =>
        string.IsNullOrWhiteSpace(Author) ? Visibility.Collapsed : Visibility.Visible;

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
