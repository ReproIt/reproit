using Avalonia.Controls;
using Avalonia.Interactivity;
using Avalonia.Markup.Xaml;

namespace AvaloniaFixture;

public partial class MainWindow : Window
{
    private bool _on;

    public MainWindow() => AvaloniaXamlLoader.Load(this);

    // Toggle flips a labeled TextBlock AND reveals/hides a detail panel. Revealing
    // the panel adds controls to the tree, so the external UIA driver observes a
    // distinct screen signature (EXPLORE:STATE + EXPLORE:EDGE), not just a
    // text-only change the structural signature intentionally ignores.
    private void OnToggle(object? sender, RoutedEventArgs e)
    {
        _on = !_on;
        Apply();
    }

    private void OnReset(object? sender, RoutedEventArgs e)
    {
        _on = false;
        Apply();
    }

    private void Apply()
    {
        var label = this.FindControl<TextBlock>("StatusLabel");
        if (label is not null)
        {
            label.Text = _on ? "Status: ON" : "Status: OFF";
        }
        var panel = this.FindControl<StackPanel>("DetailPanel");
        if (panel is not null)
        {
            panel.IsVisible = _on;
        }
    }
}
