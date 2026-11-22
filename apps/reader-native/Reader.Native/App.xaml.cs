namespace Reader.Native;

public partial class App : Application
{
    public App() => InitializeComponent();
    protected override Window CreateWindow(IActivationState? activationState)
    {
        var shell = new AppShell();
        var window = new Window(shell)
        {
            Title = "Читай — локальна бібліотека", Width = 1160, Height = 820,
            MinimumWidth = 760, MinimumHeight = 600,
        };
        window.Destroying += (_, _) =>
        {
            shell.StopListening();
            Services.AppState.Close();
        };
        return window;
    }
}
