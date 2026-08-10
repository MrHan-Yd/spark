using System.Collections.ObjectModel;
using System.Collections.Specialized;

namespace Spark.UI.Services;

/// <summary>支持批量替换的 ObservableCollection：ReplaceAll 期间抑制逐项通知，
/// 只在结束时发一次 Reset，避免搜索刷新时逐项 Add 触发 N 次布局。</summary>
public sealed class BulkObservableCollection<T> : ObservableCollection<T>
{
    private bool _bulk;

    public void ReplaceAll(IList<T> items)
    {
        _bulk = true;
        try
        {
            Items.Clear();
            foreach (var x in items)
                Items.Add(x);
        }
        finally
        {
            _bulk = false;
        }
        OnCollectionChanged(new NotifyCollectionChangedEventArgs(NotifyCollectionChangedAction.Reset));
    }

    protected override void OnCollectionChanged(NotifyCollectionChangedEventArgs e)
    {
        if (!_bulk)
            base.OnCollectionChanged(e);
    }
}
