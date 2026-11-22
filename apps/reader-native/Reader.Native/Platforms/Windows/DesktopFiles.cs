using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;

namespace Reader.Native.Services;

internal static class DesktopFiles
{
    internal static Task QueueFileAsync(StorageFile file) => DesktopFile.Supports(file.Name)
        ? DesktopImport.ReceiveAsync(file.Name, () => StageFileAsync(file))
        : Task.CompletedTask;

    private static async Task<ShareInboxItem> StageFileAsync(StorageFile file)
    {
        using var source = await file.OpenReadAsync();
        await using var stream = source.AsStreamForRead();
        return await ShareInbox.StageAsync("file", file.Name, stream);
    }

    internal static async Task<bool> QueueDropAsync(DropEventArgs args)
    {
        if (args.PlatformArgs?.DragEventArgs is not { } drag) return false;
        var deferral = drag.GetDeferral();
        var items = new List<ShareInboxItem>();
        var handled = false;
        try
        {
            var data = drag.DataView;
            if (data.Contains(StandardDataFormats.StorageItems))
            {
                foreach (var file in (await data.GetStorageItemsAsync()).OfType<StorageFile>())
                {
                    if (!DesktopFile.Supports(file.Name)) continue;
                    handled = true;
                    try { items.Add(await StageFileAsync(file)); }
                    catch (Exception error) { DesktopImport.ReportFailure(file.Name, error); }
                }
            }
            else
            {
                var address = data.Contains(StandardDataFormats.WebLink)
                    ? WebArticles.Address((await data.GetWebLinkAsync()).AbsoluteUri) : null;
                var text = address is null && data.Contains(StandardDataFormats.Text) ? await data.GetTextAsync() : null;
                handled = await DesktopImport.EnqueueDropTextAsync(address, text);
            }
        }
        finally { deferral.Complete(); }
        foreach (var item in items) DesktopImport.Publish(item);
        return handled;
    }
}
