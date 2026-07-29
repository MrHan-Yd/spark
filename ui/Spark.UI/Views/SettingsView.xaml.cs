using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Spark.UI.Views;

public sealed partial class SettingsView : UserControl
{
    public SettingsView()
    {
        InitializeComponent();
    }

    private void Back_Click(object sender, RoutedEventArgs e)
    {
        if (App.MainWindow is MainWindow win)
            win.NavigateToSearch();
    }
}
