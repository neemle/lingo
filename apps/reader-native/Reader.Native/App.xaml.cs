namespace Reader.Native;

public partial class App : Application
{
    public App() => InitializeComponent();
    protected override Window CreateWindow(IActivationState? activationState) => new(new AppShell())
    {
        Title = "Читай — локальна бібліотека", Width = 1160, Height = 820,
        MinimumWidth = 760, MinimumHeight = 600,
    };
}
