using Microsoft.UI.Dispatching;
using Microsoft.Windows.AppLifecycle;
using Reader.Native.Services;
using Windows.ApplicationModel.Activation;
using Windows.Storage;

namespace Reader.Native.WinUI;

internal static class Program
{
    [STAThread]
    private static async Task Main()
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();
        var activation = AppInstance.GetCurrent().GetActivatedEventArgs();
        var instance = AppInstance.FindOrRegisterForKey("Lingo.Reader.Main");
        if (!instance.IsCurrent)
        {
            await instance.RedirectActivationToAsync(activation);
            return;
        }

        instance.Activated += (_, args) => QueueActivation(args);
        QueueActivation(activation);
        Microsoft.UI.Xaml.Application.Start(_args =>
        {
            SynchronizationContext.SetSynchronizationContext(
                new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread()));
            _ = new App();
        });
        GC.KeepAlive(instance);
    }

    private static void QueueActivation(AppActivationArguments? args)
    {
        if (args?.Kind != ExtendedActivationKind.File || args.Data is not IFileActivatedEventArgs files) return;
        foreach (var file in files.Files.OfType<StorageFile>())
            DesktopFiles.QueueFile(file);
    }
}
