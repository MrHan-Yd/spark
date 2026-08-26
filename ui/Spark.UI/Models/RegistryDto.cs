using System;
using System.Collections.Generic;
using System.Text.Json.Serialization;
using Microsoft.UI.Xaml;

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
}

/// <summary>
/// 插件市场在 UI 展示与交互用的视图模型 DTO
/// </summary>
public sealed class RegistryPluginViewDto
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

    public string ActionButtonText
    {
        get
        {
            if (IsInstalling) return "安装中…";
            if (!IsInstalled) return "安装";
            if (CanUpdate) return "更新";
            return "已是最新";
        }
    }

    public bool ActionButtonEnabled => !IsInstalling && (!IsInstalled || CanUpdate);

    public bool IsInstalling { get; set; }

    public Visibility NativeBadgeVisibility =>
        IsNative ? Visibility.Visible : Visibility.Collapsed;

    public Visibility InstalledLabelVisibility =>
        IsInstalled ? Visibility.Visible : Visibility.Collapsed;

    public string PermissionsSummary =>
        Plugin.Permissions.Count > 0 ? string.Join(", ", Plugin.Permissions) : "无特殊权限";
}
