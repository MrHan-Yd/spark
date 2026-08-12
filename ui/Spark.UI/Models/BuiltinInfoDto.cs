using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Text.Json.Serialization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace Spark.UI.Models;

/// <summary>内置系统命令清单（host.get_builtins 返回，设置页"命令"栏展示）。</summary>
public sealed class BuiltinInfoDto : INotifyPropertyChanged
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("title")]
    public string Title { get; set; } = "";

    [JsonPropertyName("subtitle")]
    public string Subtitle { get; set; } = "";

    [JsonPropertyName("aliases")]
    public List<string> Aliases { get; set; } = new();

    /// <summary>不可逆操作（执行前会弹确认框）。</summary>
    [JsonPropertyName("confirm")]
    public bool Confirm { get; set; }

    /// <summary>图标来源路径（如 System32 下 exe/msc/cpl），UI 提取文件图标用。</summary>
    [JsonPropertyName("icon")]
    public string? IconPath { get; set; }

    [JsonIgnore]
    public Visibility ConfirmVisibility => Confirm ? Visibility.Visible : Visibility.Collapsed;

    // ===== 图标展示（字形兜底 + 系统图标覆盖，与搜索结果行一致） =====

    [JsonIgnore]
    public string IconGlyph => BuiltinIcon.GlyphFor(Id, Title);

    [JsonIgnore]
    public FontFamily IconFont => BuiltinIcon.FontFor(Id);

    [JsonIgnore]
    public double IconFontSize => BuiltinIcon.FontSizeFor(Id);

    [JsonIgnore]
    public Brush IconBrush => new SolidColorBrush(BuiltinIcon.ColorFor(Id));

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

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
