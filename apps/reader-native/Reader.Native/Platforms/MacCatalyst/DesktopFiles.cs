using Foundation;
using ImageIO;
using UIKit;

namespace Reader.Native.Services;

internal static class DesktopFiles
{
    private const int MaxClipboardImageBytes = 96 * 1024 * 1024;
    private const int MaxClipboardImagePixels = 24_000_000;

    private static readonly Dictionary<string, string> DocumentTypes = new(StringComparer.Ordinal)
    {
        ["public.plain-text"] = ".txt",
        ["org.openxmlformats.wordprocessingml.document"] = ".docx",
        ["com.adobe.pdf"] = ".pdf",
        ["public.jpeg"] = ".jpg",
        ["public.png"] = ".png",
        ["org.webmproject.webp"] = ".webp",
        ["public.heic"] = ".heic",
        ["public.heif"] = ".heif",
    };

    internal static bool QueueFile(NSUrl url)
    {
        if (FileName(url) is not { } name) return false;
        _ = DesktopImport.ReceiveAsync(name, () => StageUrlAsync(url, name));
        return true;
    }

    private static string? FileName(NSUrl url) => url.IsFileUrl && url.Path is { } path
        && Path.GetFileName(path) is { } name && DesktopFile.Supports(name) ? name : null;

    private static async Task<ShareInboxItem> StageUrlAsync(NSUrl url, string name)
    {
        var scoped = url.StartAccessingSecurityScopedResource();
        try
        {
            await using var stream = File.OpenRead(url.Path ?? throw new IOException("Не вдалося відкрити файл."));
            return await ShareInbox.StageAsync("file", name, stream).ConfigureAwait(false);
        }
        finally
        {
            if (scoped) url.StopAccessingSecurityScopedResource();
        }
    }

    internal static async Task<ShareInboxItem?> StageClipboardImageAsync()
    {
        var pasteboard = UIPasteboard.General;
        var types = pasteboard.Types ?? [];
        var type = new[] { "public.png", "public.jpeg", "public.tiff" }
            .FirstOrDefault(candidate => types.Contains(candidate, StringComparer.Ordinal));
        if (type is null)
        {
            if (pasteboard.HasImages)
                throw new InvalidDataException("Формат зображення з буфера не підтримується.");
            return null;
        }

        using var data = pasteboard.DataForPasteboardType(type)
            ?? throw new InvalidDataException("Не вдалося прочитати зображення з буфера.");
        if (data.Length > MaxClipboardImageBytes)
            throw new InvalidDataException("Зображення з буфера завелике.");

        using var converted = await Task.Run(() => PrepareClipboardImage(data, type));
        var image = converted ?? data;
        if (image.Length > Reader.CoreClient.CoreClient.MaxImportBytes)
            throw new InvalidDataException("Зображення з буфера перевищує 20 МБ.");

        using var stream = image.AsStream();
        var extension = type == "public.jpeg" ? ".jpg" : ".png";
        return await ShareInbox.StageAsync("file", "Зображення з буфера" + extension, stream);
    }

    private static NSData? PrepareClipboardImage(NSData data, string type)
    {
        using var pool = new NSAutoreleasePool();
        using var source = CGImageSource.FromData(data);
        if (source is null || source.TypeIdentifier?.ToString() != type || source.GetStatus(0) != CGImageSourceStatus.Complete)
            throw new InvalidDataException("macOS не може прочитати зображення з буфера.");

        var properties = source.GetProperties(0, null);
        var width = properties?.PixelWidth ?? 0;
        var height = properties?.PixelHeight ?? 0;
        if (width <= 0 || height <= 0 || (long)width * height > MaxClipboardImagePixels)
            throw new InvalidDataException("Розмір зображення з буфера не підтримується.");
        if (type != "public.tiff") return null;

        using var image = source.CreateThumbnail(0, new CGImageThumbnailOptions
        {
            CreateThumbnailFromImageAlways = true,
            CreateThumbnailWithTransform = true,
            MaxPixelSize = 4096,
        }) ?? throw new InvalidDataException("macOS не може прочитати зображення з буфера.");
        using var png = UIImage.FromImage(image);
        return png.AsPNG() ?? throw new InvalidDataException("Не вдалося підготувати фото з буфера.");
    }

    internal static async Task<bool> QueueDropAsync(DropEventArgs args)
    {
        if (args.PlatformArgs?.DropSession is not { } session) return false;
        var staged = new List<ShareInboxItem>();
        var handled = false;
        foreach (var item in session.Items)
        {
            var provider = item.ItemProvider;
            var suggested = Path.GetFileName(provider.SuggestedName ?? "Документ");
            var declaredType = provider.RegisteredTypeIdentifiers.FirstOrDefault(type => DocumentTypes.ContainsKey(type)
                && (type != "public.plain-text" || DesktopFile.Supports(suggested)));
            try
            {
                if (declaredType is not null)
                {
                    var filename = DesktopFile.Supports(suggested) ? suggested : suggested + DocumentTypes[declaredType];
                    staged.Add(await StageProviderAsync(provider, declaredType, filename));
                    handled = true;
                }
                else if (provider.HasItemConformingTo("public.file-url") && await StageFileItemAsync(provider) is { } original)
                {
                    staged.Add(original);
                    handled = true;
                }
            }
            catch (Exception error)
            {
                handled = true;
                DesktopImport.ReportFailure(suggested, error);
            }
        }
        foreach (var item in staged) DesktopImport.Publish(item);
        if (handled) return true;
        var address = await GetDropAddressAsync(args);
        var text = address is null ? await GetDropTextAsync(args) : null;
        return await DesktopImport.EnqueueDropTextAsync(address, text);
    }

    private static Task<ShareInboxItem?> StageFileItemAsync(NSItemProvider provider)
    {
        var completion = new TaskCompletionSource<ShareInboxItem?>(TaskCreationOptions.RunContinuationsAsynchronously);
        provider.LoadItem("public.file-url", null, (value, error) =>
        {
            if (error is not null) completion.TrySetException(new IOException(error.LocalizedDescription));
            else if (value is NSUrl url && FileName(url) is { } name) StageInCallback(completion, url, name);
            else completion.TrySetResult(null);
        });
        return completion.Task;
    }

    private static async Task<Uri?> GetDropAddressAsync(DropEventArgs args)
    {
        if (args.PlatformArgs?.DropSession is not { } session) return null;
        foreach (var item in session.Items)
        {
            var provider = item.ItemProvider;
            if (provider.HasItemConformingTo("public.file-url") || !provider.HasItemConformingTo("public.url")) continue;
            var completion = new TaskCompletionSource<Uri?>(TaskCreationOptions.RunContinuationsAsynchronously);
            provider.LoadItem("public.url", null, (value, error) =>
                completion.TrySetResult(error is null && value is NSUrl url ? WebArticles.Address(url.AbsoluteString) : null));
            if (await completion.Task is { } address) return address;
        }
        return null;
    }

    private static async Task<string?> GetDropTextAsync(DropEventArgs args)
    {
        if (args.PlatformArgs?.DropSession is not { } session) return null;
        foreach (var item in session.Items)
        {
            var provider = item.ItemProvider;
            if (provider.HasItemConformingTo("public.file-url") ||
                provider.SuggestedName is { } name && DesktopFile.Supports(name)) continue;
            var type = provider.HasItemConformingTo("public.utf8-plain-text") ? "public.utf8-plain-text" : "public.plain-text";
            if (!provider.HasItemConformingTo(type)) continue;
            var completion = new TaskCompletionSource<string?>(TaskCreationOptions.RunContinuationsAsynchronously);
            provider.LoadItem(type, null, (value, error) => completion.TrySetResult(error is null ? value switch
            {
                NSString nativeText => nativeText.ToString(),
                NSData data when data.Length <= Reader.CoreClient.CoreClient.MaxImportBytes =>
                    System.Text.Encoding.UTF8.GetString(data.ToArray()),
                _ => null,
            } : null));
            if (await completion.Task is { Length: > 0 } text) return text;
        }
        return null;
    }

    private static async Task<ShareInboxItem> StageProviderAsync(NSItemProvider provider, string type, string name)
    {
        var completion = new TaskCompletionSource<ShareInboxItem?>(TaskCreationOptions.RunContinuationsAsynchronously);
        provider.LoadInPlaceFileRepresentation(type, (url, _, error) =>
        {
            if (error is not null || url is null)
            {
                completion.TrySetException(new IOException(error?.LocalizedDescription ?? "Не вдалося прочитати перетягнутий файл."));
                return;
            }
            StageInCallback(completion, url, name);
        });
        return (await completion.Task.ConfigureAwait(false))!;
    }

    private static void StageInCallback(TaskCompletionSource<ShareInboxItem?> completion, NSUrl url, string name)
    {
        // A provider may invalidate its temporary URL as soon as this callback returns.
        try { completion.TrySetResult(StageUrlAsync(url, name).GetAwaiter().GetResult()); }
        catch (Exception error) { completion.TrySetException(error); }
    }
}
