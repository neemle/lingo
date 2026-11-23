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
        void Activate(object? sender, EventArgs args)
        {
            window.Activated -= Activate;
            window.Dispatcher.Dispatch(shell.RestoreAfterActivation);
        }
        window.Activated += Activate;
        window.Destroying += (_, _) =>
        {
            window.Activated -= Activate;
            shell.StopListening();
            if (shell.CurrentPage is Features.Reading.ReadingPage reading) reading.PrepareWindowClose();
            Services.AppState.StopTranslationWorker();
        };
        return window;
    }
}
