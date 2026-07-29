using System.Collections.ObjectModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Spark.UI.Models;
using Windows.System;

namespace Spark.UI.Views;

public sealed partial class SearchView : UserControl
{
    private readonly ObservableCollection<CandidateDto> _items = new();
    private DispatcherTimer? _debounce;

    public SearchView()
    {
        InitializeComponent();
        ResultsList.ItemsSource = _items;
        Loaded += SearchView_Loaded;
    }

    private async void SearchView_Loaded(object sender, RoutedEventArgs e)
    {
        FocusQueryBox();
        await RunQueryAsync(string.Empty);
    }

    public void FocusQueryBox()
    {
        QueryBox.Focus(FocusState.Programmatic);
    }

    private void BtnSettings_Click(object sender, RoutedEventArgs e)
    {
        if (App.MainWindow is MainWindow win)
            win.NavigateToSettings();
    }

    private void QueryBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        _debounce?.Stop();
        _debounce = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(50) };
        _debounce.Tick += async (_, _) =>
        {
            _debounce.Stop();
            await RunQueryAsync(QueryBox.Text ?? string.Empty);
        };
        _debounce.Start();
    }

    private async void QueryBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter)
        {
            e.Handled = true;
            if (ResultsList.SelectedItem is CandidateDto c)
                await InvokeAsync(c);
            else if (_items.Count > 0)
                await InvokeAsync(_items[0]);
        }
        else if (e.Key == VirtualKey.Escape)
        {
            e.Handled = true;
            if (!string.IsNullOrEmpty(QueryBox.Text))
                QueryBox.Text = string.Empty;
        }
        else if (e.Key == VirtualKey.Down && _items.Count > 0)
        {
            e.Handled = true;
            ResultsList.SelectedIndex = Math.Min(Math.Max(ResultsList.SelectedIndex, 0) + 1, _items.Count - 1);
        }
        else if (e.Key == VirtualKey.Up && _items.Count > 0)
        {
            e.Handled = true;
            ResultsList.SelectedIndex = Math.Max(ResultsList.SelectedIndex - 1, 0);
        }
    }

    private async void ResultsList_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is CandidateDto c)
            await InvokeAsync(c);
    }

    private async Task RunQueryAsync(string text)
    {
        try
        {
            QueryResultDto result;
            if (App.MainWindow is MainWindow win)
                result = await win.Ipc.QueryAsync(text);
            else
                result = Services.DemoData.Query(text);

            _items.Clear();
            foreach (var item in result.Items)
                _items.Add(item);

            var online = App.MainWindow is MainWindow w && w.Ipc.IsConnected;
            FooterStatus.Text = online
                ? $"Host 在线 · {_items.Count} 项"
                : $"离线演示 · {_items.Count} 项";

            if (_items.Count > 0)
                ResultsList.SelectedIndex = 0;
        }
        catch (Exception ex)
        {
            FooterStatus.Text = "查询失败: " + ex.Message;
        }
    }

    private async Task InvokeAsync(CandidateDto item)
    {
        if (item.Id == "sys.settings" || (item.Title?.Contains("设置") ?? false))
        {
            if (App.MainWindow is MainWindow win)
                win.NavigateToSettings();
            return;
        }

        if (App.MainWindow is MainWindow main)
        {
            try
            {
                await main.Ipc.InvokeAsync(item.Id, "open", QueryBox.Text ?? "");
            }
            catch
            {
                // offline ok
            }
        }

        FooterStatus.Text = "已执行: " + item.Title;
    }
}
