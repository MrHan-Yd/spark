using System.Text.Json.Serialization;

namespace Spark.UI.Models;

/// <summary>
/// host.plugin.install 返回：告知是新装、覆盖更新还是需要确认降级。
/// 字段名与 <c>spark_plugin_manager::PluginInstallOutcome</c> 的 serde 形状对应（snake_case）。
/// </summary>
public sealed class PluginInstallOutcomeDto
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    /// <summary>installed | updated | confirm_downgrade</summary>
    [JsonPropertyName("action")]
    public string Action { get; set; } = "";

    [JsonPropertyName("version")]
    public string Version { get; set; } = "";

    /// <summary>旧版本号；全新装为 null。</summary>
    [JsonPropertyName("previous_version")]
    public string? PreviousVersion { get; set; }
}