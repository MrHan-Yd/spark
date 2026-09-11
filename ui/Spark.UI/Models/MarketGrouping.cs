using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Spark.UI.Models;

/// <summary>
/// 市场列表的分类分组头（作为一行插进列表数据源；规范 §3.3 的 tags）。
/// 市场列表保持扁平 ItemsSource + 行模板选择器的组合，而不是换成
/// CollectionViewSource.IsSourceGrouped：列表的「筛选后原位回填安装状态、不重建容器」
/// 逻辑（ApplyMarketFilter 的 skipReassign）依赖扁平数据源，换成分组源会打断
/// 安装中卡片的进度动画与滚动位置。
/// </summary>
public sealed class MarketGroupHeaderVm
{
    public MarketGroupHeaderVm(string tag, int count)
    {
        Tag = tag;
        Count = count;
    }

    /// <summary>分组名（标签原文；无标签插件为「未分类」）。</summary>
    public string Tag { get; }

    /// <summary>该组插件数。</summary>
    public int Count { get; }

    /// <summary>组头文案：「开发工具 · 3」。</summary>
    public string HeaderText => $"{Tag} · {Count}";
}

/// <summary>
/// 市场列表行模板选择器：分组头行走组头模板，插件行走卡片模板。
/// </summary>
public sealed partial class MarketRowTemplateSelector : DataTemplateSelector
{
    public DataTemplate? HeaderTemplate { get; set; }
    public DataTemplate? CardTemplate { get; set; }

    protected override DataTemplate SelectTemplateCore(object item)
        => item is MarketGroupHeaderVm ? HeaderTemplate! : CardTemplate!;

    protected override DataTemplate SelectTemplateCore(object item, DependencyObject container)
        => SelectTemplateCore(item);
}

/// <summary>
/// 市场分类 chip 的配色组。由 MainWindow 从 <c>Root.Resources</c> 取现成画刷传入——
/// 主题切换是原地改这些 SolidColorBrush 的 Color（见 ApplyTheme），引用保持有效，
/// 所以 chip 会跟着深/浅主题走，不需要为它单独订阅主题变更。
/// </summary>
public sealed class MarketChipPalette
{
    public MarketChipPalette(Brush bgOff, Brush borderOff, Brush fgOff,
        Brush bgOn, Brush borderOn, Brush fgOn)
    {
        BgOff = bgOff;
        BorderOff = borderOff;
        FgOff = fgOff;
        BgOn = bgOn;
        BorderOn = borderOn;
        FgOn = fgOn;
    }

    public Brush BgOff { get; }
    public Brush BorderOff { get; }
    public Brush FgOff { get; }
    public Brush BgOn { get; }
    public Brush BorderOn { get; }
    public Brush FgOn { get; }
}

/// <summary>
/// 市场「分类」筛选栏的一枚 chip。<see cref="Tag"/> 为 null 表示「全部」。
/// </summary>
public sealed class MarketTagChipVm : INotifyPropertyChanged
{
    private readonly MarketChipPalette _palette;
    private bool _isSelected;

    public MarketTagChipVm(string? tag, string label, int count, MarketChipPalette palette)
    {
        Tag = tag;
        Label = count > 0 ? $"{label} {count}" : label;
        _palette = palette;
    }

    /// <summary>该 chip 对应的标签；null = 不做标签过滤（「全部」）。</summary>
    public string? Tag { get; }

    /// <summary>按钮文案（带该分类下的插件数）。</summary>
    public string Label { get; }

    public bool IsSelected
    {
        get => _isSelected;
        set
        {
            if (_isSelected == value) return;
            _isSelected = value;
            OnPropertyChanged(nameof(IsSelected));
            OnPropertyChanged(nameof(ChipBackground));
            OnPropertyChanged(nameof(ChipBorderBrush));
            OnPropertyChanged(nameof(ChipForeground));
        }
    }

    public Brush ChipBackground => _isSelected ? _palette.BgOn : _palette.BgOff;
    public Brush ChipBorderBrush => _isSelected ? _palette.BorderOn : _palette.BorderOff;
    public Brush ChipForeground => _isSelected ? _palette.FgOn : _palette.FgOff;

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
