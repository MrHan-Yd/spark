using System.Text.Json.Serialization;

namespace Spark.UI.Models;

/// <summary>
/// host.plugin.list 返回项。字段名与 <c>spark_plugin_manager::PluginInfo</c> 的
/// serde 形状一一对应（snake_case）。
/// </summary>
public sealed class PluginInfoDto
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    [JsonPropertyName("version")]
    public string Version { get; set; } = "";

    [JsonPropertyName("api_version")]
    public uint ApiVersion { get; set; }

    /// <summary>webview | native | wasm</summary>
    [JsonPropertyName("runtime")]
    public string Runtime { get; set; } = "webview";

    [JsonPropertyName("description")]
    public string? Description { get; set; }

    [JsonPropertyName("author")]
    public string? Author { get; set; }

    /// <summary>图标绝对路径（host 已拼好），无图标为 null。</summary>
    [JsonPropertyName("icon")]
    public string? Icon { get; set; }

    [JsonPropertyName("homepage")]
    public string? Homepage { get; set; }

    /// <summary>清单声明的权限。</summary>
    [JsonPropertyName("permissions")]
    public List<string> Permissions { get; set; } = new();

    /// <summary>用户已授权的权限（Permissions 的子集）。</summary>
    [JsonPropertyName("granted")]
    public List<string> Granted { get; set; } = new();

    [JsonPropertyName("enabled")]
    public bool Enabled { get; set; }

    /// <summary>standard | dev</summary>
    [JsonPropertyName("source")]
    public string Source { get; set; } = "standard";

    [JsonPropertyName("features")]
    public List<PluginFeatureDto> Features { get; set; } = new();

    /// <summary>
    /// 签名状态：host <c>PluginInfo.sign_state</c> 的 snake_case 形状。
    /// <c>official</c> / <c>third_party</c> / <c>unsigned</c> / <c>invalid</c>。
    /// 用字符串承接反序列化（与 <see cref="Runtime"/>/<see cref="Source"/> 一致），
    /// 由 <see cref="ViewModels.PluginRowVm"/> 解析为 <see cref="PluginSignState"/>。
    /// </summary>
    [JsonPropertyName("sign_state")]
    public string SignState { get; set; } = "unsigned";
}

/// <summary>
/// 插件签名状态（纯展示用枚举，不直接参与 JSON 反序列化）。
/// 对应 host <c>spark_plugin_manager::SignState</c>。
/// </summary>
public enum PluginSignState
{
    /// <summary>官方密钥验签通过。</summary>
    Official,
    /// <summary>三方密钥验签通过（v1 暂不产生）。</summary>
    ThirdParty,
    /// <summary>无 signature.json（host 老版本未返回 sign_state 时亦回落此值）。</summary>
    Unsigned,
    /// <summary>有 signature.json 但验签失败。</summary>
    Invalid,
}

public sealed class PluginFeatureDto
{
    /// <summary>keyword | regex | root</summary>
    [JsonPropertyName("type")]
    public string Kind { get; set; } = "keyword";

    [JsonPropertyName("keyword")]
    public string? Keyword { get; set; }

    [JsonPropertyName("pattern")]
    public string? Pattern { get; set; }

    [JsonPropertyName("title")]
    public string Title { get; set; } = "";

    [JsonPropertyName("subtitle")]
    public string? Subtitle { get; set; }

    /// <summary>page | list</summary>
    [JsonPropertyName("mode")]
    public string Mode { get; set; } = "page";

    [JsonPropertyName("placeholder")]
    public string? Placeholder { get; set; }
}

/// <summary>host.plugin.open 返回：打开 WebView2 插件窗口所需的一切。</summary>
public sealed class PluginOpenInfoDto
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    /// <summary>插件显示名（清单 name），用于窗口标题栏。</summary>
    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    /// <summary>页面入口（index.html）绝对路径。</summary>
    [JsonPropertyName("main_abs")]
    public string MainAbs { get; set; } = "";

    [JsonPropertyName("window")]
    public PluginWindowDto Window { get; set; } = new();

    [JsonPropertyName("permissions")]
    public List<string> Permissions { get; set; } = new();

    [JsonPropertyName("granted")]
    public List<string> Granted { get; set; } = new();

    /// <summary>清单声明且文件存在时的自定义 preload.js 绝对路径。</summary>
    [JsonPropertyName("preload_abs")]
    public string? PreloadAbs { get; set; }

    /// <summary>插件根目录绝对路径，用作 WebView2 虚拟主机映射的物理目录。</summary>
    [JsonPropertyName("root")]
    public string Root { get; set; } = "";

    /// <summary>图标绝对路径（清单 icon 拼 root，文件存在时）；null 时 UI 降级到内置 spark 图标。</summary>
    [JsonPropertyName("icon_abs")]
    public string? IconAbs { get; set; }
}

public sealed class PluginWindowDto
{
    [JsonPropertyName("width")]
    public uint Width { get; set; } = 480;

    [JsonPropertyName("height")]
    public uint Height { get; set; } = 360;

    [JsonPropertyName("min_width")]
    public uint MinWidth { get; set; } = 240;

    [JsonPropertyName("min_height")]
    public uint MinHeight { get; set; } = 180;

    [JsonPropertyName("resizable")]
    public bool Resizable { get; set; } = true;

    [JsonPropertyName("always_on_top")]
    public bool AlwaysOnTop { get; set; }

    [JsonPropertyName("frame")]
    public bool Frame { get; set; } = true;
}
