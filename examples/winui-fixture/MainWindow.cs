using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace WinUiFixture;

// Build this tiny validation surface directly instead of loading MainWindow.xaml.
// On unpackaged, self-contained launches, Microsoft.UI.Xaml can fail while
// resolving the compiled Window resource before a top-level window is exposed.
public sealed class MainWindow : Window
{
    private readonly TextBlock _statusLabel;
    private readonly StackPanel _detailPanel;
    private bool _on;

    public MainWindow()
    {
        Title = "ReproIt WinUI Fixture";

        _statusLabel = new TextBlock { Text = "Status: OFF" };
        AutomationProperties.SetAutomationId(_statusLabel, "statusLabel");

        var toggleButton = new Button { Content = "Toggle" };
        AutomationProperties.SetAutomationId(toggleButton, "toggleButton");
        toggleButton.Click += OnToggle;

        var detailLabel = new TextBlock { Text = "Detail panel revealed" };
        AutomationProperties.SetAutomationId(detailLabel, "detailLabel");

        var resetButton = new Button { Content = "Reset" };
        AutomationProperties.SetAutomationId(resetButton, "resetButton");
        resetButton.Click += OnReset;

        _detailPanel = new StackPanel
        {
            Visibility = Visibility.Collapsed,
            Spacing = 8,
        };
        AutomationProperties.SetAutomationId(_detailPanel, "detailPanel");
        _detailPanel.Children.Add(detailLabel);
        _detailPanel.Children.Add(resetButton);

        var root = new StackPanel
        {
            Margin = new Thickness(24),
            Spacing = 16,
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Top,
        };
        root.Children.Add(_statusLabel);
        root.Children.Add(toggleButton);
        root.Children.Add(_detailPanel);
        Content = root;
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
        _statusLabel.Text = _on ? "Status: ON" : "Status: OFF";
        _detailPanel.Visibility = _on ? Visibility.Visible : Visibility.Collapsed;
    }
}
