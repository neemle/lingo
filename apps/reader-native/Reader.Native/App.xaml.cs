namespace Reader.Native;

public partial class App : Application
{
    public App() => InitializeComponent();
    protected override Window CreateWindow(IActivationState? activationState)
    {
        var shell = new AppShell();
        var window = new Window(shell)
        {
            Title = "Читай — локальна бібліотека",
        };
#if IOS
        shell.FlyoutBehavior = FlyoutBehavior.Flyout;
#else
        window.Width = 1160;
        window.Height = 820;
        window.MinimumWidth = 760;
        window.MinimumHeight = 600;
#endif
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
