using Foundation;
using UIKit;

namespace Reader.Native.Services;

internal static class NativeFileSave
{
    internal static async Task<bool> SaveAsync(Microsoft.Maui.Controls.Window window, string name, byte[] bytes)
    {
        if (window.Handler?.PlatformView is not UIWindow { RootViewController: { } presenter })
            throw new InvalidOperationException("Вікно застосунку недоступне.");
        while (presenter.PresentedViewController is { } presented) presenter = presented;

        var directory = Directory.CreateTempSubdirectory("lingo-export-");
        try
        {
            var path = Path.Combine(directory.FullName, Path.GetFileName(name));
            await File.WriteAllBytesAsync(path, bytes);
            using var url = NSUrl.FromFilename(path);
            using var picker = new UIDocumentPickerViewController([url], asCopy: true);
            var result = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            void Closed(object? sender, EventArgs args) => result.TrySetResult(false);
            picker.DidPickDocumentAtUrls += (_, args) => result.TrySetResult(args.Urls.Length != 0);
            picker.WasCancelled += Closed;
            window.Destroying += Closed;
            try
            {
                presenter.PresentViewController(picker, animated: true, completionHandler: null);
                return await result.Task;
            }
            finally { window.Destroying -= Closed; }
        }
        finally { directory.Delete(recursive: true); }
    }
}
