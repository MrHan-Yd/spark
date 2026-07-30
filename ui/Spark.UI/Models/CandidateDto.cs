using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Text.Json.Serialization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace Spark.UI.Models;

public sealed class CandidateDto : INotifyPropertyChanged
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("title")]
    public string Title { get; set; } = "";

    [JsonPropertyName("subtitle")]
    public string? Subtitle { get; set; }

    [JsonPropertyName("target")]
    public string? Target { get; set; }

    [JsonPropertyName("icon")]
    public string? IconPath { get; set; }

    [JsonPropertyName("score")]
    public float Score { get; set; }

    /// <summary>Wire: app|file|history|favorite|plugin|builtin</summary>
    [JsonPropertyName("source")]
    public string Source { get; set; } = "app";

    [JsonPropertyName("plugin_id")]
    public string? PluginId { get; set; }

    [JsonIgnore]
    public string Kind => Source switch
    {
        "file" => "file",
        "history" => "history",
        "favorite" => "history",
        "plugin" => "plugin",
        "builtin" => "system",
        _ => "app",
    };

    [JsonIgnore]
    public string SourceLabel => Source switch
    {
        "app" => "应用",
        "file" => "文件",
        "history" => "历史",
        "favorite" => "收藏",
        "plugin" => "插件",
        "builtin" => "系统",
        _ => Source,
    };

    /// <summary>Letter fallback when no system icon.</summary>
    [JsonIgnore]
    public string IconGlyph
    {
        get
        {
            if (!string.IsNullOrEmpty(Title))
            {
                var ch = Title.Trim()[0];
                return char.IsLetterOrDigit(ch) ? ch.ToString().ToUpperInvariant() : "•";
            }
            return "?";
        }
    }

    [JsonIgnore]
    public string Shortcut { get; set; } = "";

    private ImageSource? _iconImage;
    [JsonIgnore]
    public ImageSource? IconImage
    {
        get => _iconImage;
        set
        {
            _iconImage = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(HasIconImage));
            OnPropertyChanged(nameof(IconImageVisibility));
            OnPropertyChanged(nameof(IconTextVisibility));
        }
    }

    [JsonIgnore]
    public bool HasIconImage => IconImage is not null;

    [JsonIgnore]
    public Visibility IconImageVisibility => HasIconImage ? Visibility.Visible : Visibility.Collapsed;

    [JsonIgnore]
    public Visibility IconTextVisibility => HasIconImage ? Visibility.Collapsed : Visibility.Visible;

    [JsonIgnore]
    public Brush IconBrush
    {
        get
        {
            var c = Kind switch
            {
                "file" => Color.FromArgb(255, 48, 176, 199),
                "plugin" => Color.FromArgb(255, 191, 90, 242),
                "history" => Color.FromArgb(255, 255, 159, 10),
                "calc" => Color.FromArgb(255, 48, 209, 88),
                "system" => Color.FromArgb(255, 10, 132, 255),
                _ => Color.FromArgb(255, 0, 122, 255),
            };
            return new SolidColorBrush(c);
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}

public sealed class QueryResultDto
{
    [JsonPropertyName("items")]
    public List<CandidateDto> Items { get; set; } = new();

    [JsonPropertyName("partial")]
    public bool Partial { get; set; }
}
