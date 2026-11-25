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
        _ = ReceiveSharedFilesAsync();
        window.Activated += ReceiveSharedFiles;
        window.Deactivated += (_, _) => Services.AppState.SetTranslationForeground(false);
        window.Activated += (_, _) => Services.AppState.SetTranslationForeground(true);
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
#if IOS
            window.Activated -= ReceiveSharedFiles;
#endif
            window.Activated -= Activate;
            shell.StopListening();
            if (shell.CurrentPage is Features.Reading.ReadingPage reading) reading.PrepareWindowClose();
            Services.AppState.StopTranslationWorker();
        };
        return window;

#if IOS
        void ReceiveSharedFiles(object? sender, EventArgs args) => _ = ReceiveSharedFilesAsync();

        async Task ReceiveSharedFilesAsync()
        {
            var batch = await Task.Run(Services.IosSharedImports.ReceiveAsync).ConfigureAwait(false);
            if (batch is not null)
                window.Dispatcher.Dispatch(() =>
                {
                    if (!shell.IsClosing) Services.DesktopImport.PublishBatch(batch.Items, batch.Rejected);
                });
        }
#endif
    }
}
