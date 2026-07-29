using System.Text.Json.Serialization;

namespace Spark.UI.Models;

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
    public string Source { get; set; } = "应用";

    [JsonPropertyName("icon")]
    public string Icon { get; set; } = "?";
}

public sealed class QueryResultDto
{
    [JsonPropertyName("items")]
    public List<CandidateDto> Items { get; set; } = new();
}
