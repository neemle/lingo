using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Storage.Streams;
using WindowsClipboard = Windows.ApplicationModel.DataTransfer.Clipboard;
using WindowsDataPackageView = Windows.ApplicationModel.DataTransfer.DataPackageView;

namespace Reader.Native.Services;

internal static class DesktopFiles
{
    private const string DroppedImageName = "Перетягнуте зображення";

    internal static Task QueueFilesAsync(IEnumerable<StorageFile> files) => DesktopImport.ReceiveBatchAsync(files
        .Where(file => DesktopFile.Supports(file.Name))
        .Select(file => (file.Name, new Func<Task<ShareInboxItem>>(() => StageFileAsync(file)))));

    internal static async Task<ShareInboxItem?> StageClipboardImageAsync()
    {
        var data = WindowsClipboard.GetContent();
        return data.Contains(StandardDataFormats.Bitmap)
            ? await StageBitmapAsync(data, "Зображення з буфера") : null;
    }

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
        var rejected = new List<(string Name, Exception Error)>();
        var handled = false;
        try
        {
            var data = drag.DataView;
            var containsFiles = data.Contains(StandardDataFormats.StorageItems);
            if (containsFiles)
            {
                foreach (var file in (await data.GetStorageItemsAsync()).OfType<StorageFile>())
                {
                    if (!DesktopFile.Supports(file.Name)) continue;
                    handled = true;
                    try { items.Add(await StageFileAsync(file)); }
                    catch (Exception error) { rejected.Add((file.Name, error)); }
                }
            }
            if (!handled && data.Contains(StandardDataFormats.Bitmap))
            {
                handled = true;
                try { items.Add(await StageBitmapAsync(data, DroppedImageName)); }
                catch (Exception error) { rejected.Add((DroppedImageName, error)); }
            }
            else if (!containsFiles)
            {
                var address = data.Contains(StandardDataFormats.WebLink)
                    ? WebArticles.Address((await data.GetWebLinkAsync()).AbsoluteUri) : null;
                var text = address is null && data.Contains(StandardDataFormats.Text) ? await data.GetTextAsync() : null;
                handled = await DesktopImport.EnqueueDropTextAsync(address, text);
            }
        }
        finally { deferral.Complete(); }
        DesktopImport.PublishBatch(items, rejected);
        return handled;
    }

    private static async Task<ShareInboxItem> StageBitmapAsync(WindowsDataPackageView data, string name)
    {
        var reference = await data.GetBitmapAsync();
        using var source = await reference.OpenReadAsync();
        if (source.Size is 0 or > Reader.CoreClient.CoreClient.MaxImportBytes)
            throw new InvalidDataException("Зображення порожнє або перевищує 20 МБ.");

        var decoder = await BitmapDecoder.CreateAsync(source);
        var pixels = (ulong)decoder.PixelWidth * decoder.PixelHeight;
        if (pixels is 0 or > 64_000_000)
            throw new InvalidDataException("Непідтримуваний розмір зображення.");

        var codec = decoder.DecoderInformation.CodecId;
        if (codec == BitmapDecoder.PngDecoderId) return await StageImageAsync(source, name + ".png");
        if (codec == BitmapDecoder.JpegDecoderId) return await StageImageAsync(source, name + ".jpg");

        // Convert formats such as BMP to a bounded PNG that the photo inbox can reopen.
        var scale = Math.Min(1.0, 2048.0 / Math.Max(decoder.PixelWidth, decoder.PixelHeight));
        var transform = new BitmapTransform
        {
            ScaledWidth = (uint)Math.Max(1, Math.Floor(decoder.PixelWidth * scale)),
            ScaledHeight = (uint)Math.Max(1, Math.Floor(decoder.PixelHeight * scale)),
        };
        using var bitmap = await decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Straight,
            transform, ExifOrientationMode.RespectExifOrientation, ColorManagementMode.DoNotColorManage);
        using var encoded = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, encoded);
        encoder.SetSoftwareBitmap(bitmap);
        await encoder.FlushAsync();
        return await StageImageAsync(encoded, name + ".png");
    }

    private static async Task<ShareInboxItem> StageImageAsync(IRandomAccessStream source, string name)
    {
        source.Seek(0);
        await using var stream = source.AsStreamForRead();
        return await ShareInbox.StageAsync("file", name, stream);
    }
}
