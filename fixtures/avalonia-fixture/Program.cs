using System;
using Avalonia;

namespace AvaloniaFixture;

// Minimal Avalonia desktop entry point. Drives the classic desktop lifetime so
// a real top-level Win32 window is created and published to UI Automation, which
// is exactly what the production `reproit __uia` backend walks.
internal static class Program
{
    [STAThread]
    public static void Main(string[] args) =>
        BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);

    public static AppBuilder BuildAvaloniaApp() =>
        AppBuilder.Configure<App>()
            .UsePlatformDetect()
            .LogToTrace();
}
