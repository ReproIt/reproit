using System;
using System.Windows;

namespace WpfFixture;

public sealed class App : Application
{
    [STAThread]
    public static void Main() => new App().Run(new MainWindow());
}
