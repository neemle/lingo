using Foundation;
using QuickLook;
using UIKit;

namespace Reader.Native.Services;

internal static class NativeFilePreview
{
    internal static async Task ShowAsync(Microsoft.Maui.Controls.Window window, string path)
    {
        if (window.Handler?.PlatformView is not UIWindow { RootViewController: { } presenter })
            throw new InvalidOperationException("Вікно застосунку недоступне.");
        while (presenter.PresentedViewController is { } presented) presenter = presented;

        using var url = NSUrl.FromFilename(path);
        using var item = new OriginalPreviewItem(url);
        if (!QLPreviewController.CanPreviewItem(item))
            throw new InvalidOperationException("Система не може показати цей документ.");
        using var source = new OriginalPreviewSource(item);
        using var preview = new QLPreviewController { DataSource = source };
        var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        void Closed(object? sender, EventArgs args) => completion.TrySetResult();
        preview.DidDismiss += Closed;
        window.Destroying += Closed;
        try
        {
            presenter.PresentViewController(preview, animated: true, completionHandler: null);
            await completion.Task;
        }
        finally { window.Destroying -= Closed; }
    }

    private sealed class OriginalPreviewItem(NSUrl url) : QLPreviewItem
    {
        public override NSUrl PreviewItemUrl => url;
    }

    private sealed class OriginalPreviewSource(QLPreviewItem item) : QLPreviewControllerDataSource
    {
        public override nint PreviewItemCount(QLPreviewController controller) => 1;
        public override IQLPreviewItem GetPreviewItem(QLPreviewController controller, nint index) => item;
    }
}
