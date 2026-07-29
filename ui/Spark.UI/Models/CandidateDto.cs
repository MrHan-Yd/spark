using System.Text.Json.Serialization;

namespace Spark.UI.Models;

/// <summary>对齐 crates/ipc + spark-core Candidate JSON。</summary>
public sealed class CandidateDto
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("title")]
    public string Title { get; set; } = "";

    [JsonPropertyName("subtitle")]
    public string? Subtitle { get; set; }

    [JsonPropertyName("score")]
    public float Score { get; set; }

    [JsonPropertyName("source")]
    public string Source { get; set; } = "";

    [JsonPropertyName("plugin_id")]
    public string? PluginId { get; set; }
}

public sealed class QueryResultDto
{
    [JsonPropertyName("items")]
    public List<CandidateDto> Items { get; set; } = new();

    [JsonPropertyName("partial")]
    public bool Partial { get; set; }
}
