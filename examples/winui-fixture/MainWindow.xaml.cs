using Microsoft.UI.Xaml;

namespace WinUiFixture;

// Minimal WinUI 3 window matching the Avalonia/WPF fixture shape: a Toggle button
// that flips a labeled TextBlock and reveals/hides a detail panel. Revealing the
// panel adds controls to the tree, so the external UIA driver
// (`reproit __uia`) observes a distinct screen signature (EXPLORE:STATE +
// EXPLORE:EDGE), not just a text-only change the structural signature ignores.
public sealed partial class MainWindow : Window
{
    private bool _on;

    public MainWindow()
    {
        InitializeComponent();
    }

    private void OnToggle(object sender, RoutedEventArgs e)
    {
        _on = !_on;
        Apply();
    }

    private void OnReset(object sender, RoutedEventArgs e)
    {
        _on = false;
        Apply();
    }

    private void Apply()
    {
        StatusLabel.Text = _on ? "Status: ON" : "Status: OFF";
        DetailPanel.Visibility = _on ? Visibility.Visible : Visibility.Collapsed;
    }
}
