using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;

namespace Reader.Native.Services;

internal static class DesktopFiles
{
    internal static bool QueueFile(StorageFile file) => DesktopImport.Enqueue(new DesktopFile(file.Name, async () =>
    {
        using var source = await file.OpenReadAsync();
        await using var stream = source.AsStreamForRead();
        return await NativeFiles.ReadBoundedAsync(stream, Reader.CoreClient.CoreClient.MaxImportBytes);
    }));

    internal static async Task<int> QueueDropAsync(DropEventArgs args)
    {
        var data = args.PlatformArgs?.DragEventArgs.DataView;
        if (data is null || !data.Contains(StandardDataFormats.StorageItems)) return 0;
        return (await data.GetStorageItemsAsync()).OfType<StorageFile>().Count(QueueFile);
    }

    internal static async Task<Uri?> GetDropAddressAsync(DropEventArgs args)
    {
        var data = args.PlatformArgs?.DragEventArgs.DataView;
        if (data is null || data.Contains(StandardDataFormats.StorageItems)) return null;
        if (data.Contains(StandardDataFormats.WebLink))
            return WebArticles.Address((await data.GetWebLinkAsync()).AbsoluteUri);
        return null;
    }

    internal static async Task<string?> GetDropTextAsync(DropEventArgs args)
    {
        var data = args.PlatformArgs?.DragEventArgs.DataView;
        if (data is null || data.Contains(StandardDataFormats.StorageItems) || !data.Contains(StandardDataFormats.Text))
            return null;
        return await data.GetTextAsync();
    }
}
