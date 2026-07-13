using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;

namespace WpfFixture;

public sealed class MainWindow : Window
{
    private readonly TextBlock statusLabel;
    private readonly StackPanel detailPanel;

    public MainWindow()
    {
        Title = "ReproIt WPF Fixture";
        Width = 360;
        Height = 260;

        var root = new StackPanel { Margin = new Thickness(24) };
        statusLabel = new TextBlock { Text = "Status: OFF", Margin = new Thickness(0, 0, 0, 16) };
        AutomationProperties.SetAutomationId(statusLabel, "statusLabel");

        var toggle = new Button { Content = "Toggle", Width = 120, HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 0, 0, 16) };
        AutomationProperties.SetAutomationId(toggle, "toggleButton");
        toggle.Click += (_, _) => SetExpanded(true);

        detailPanel = new StackPanel { Visibility = Visibility.Collapsed };
        AutomationProperties.SetAutomationId(detailPanel, "detailPanel");
        var detail = new TextBlock { Text = "Detail panel revealed", Margin = new Thickness(0, 0, 0, 8) };
        AutomationProperties.SetAutomationId(detail, "detailLabel");
        var reset = new Button { Content = "Reset", Width = 120, HorizontalAlignment = HorizontalAlignment.Left };
        AutomationProperties.SetAutomationId(reset, "resetButton");
        reset.Click += (_, _) => SetExpanded(false);
        detailPanel.Children.Add(detail);
        detailPanel.Children.Add(reset);

        root.Children.Add(statusLabel);
        root.Children.Add(toggle);
        root.Children.Add(detailPanel);
        Content = root;
    }

    private void SetExpanded(bool expanded)
    {
        statusLabel.Text = expanded ? "Status: ON" : "Status: OFF";
        detailPanel.Visibility = expanded ? Visibility.Visible : Visibility.Collapsed;
    }
}
