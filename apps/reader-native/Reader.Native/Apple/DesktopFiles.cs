using System.Text;
using Foundation;
using ImageIO;
using UIKit;

namespace Reader.Native.Services;

internal static class DesktopFiles
{
    private const int MaxClipboardImageBytes = 96 * 1024 * 1024;
    private const int MaxClipboardImagePixels = 24_000_000;
    private static readonly string[] ClipboardImageTypes =
        ["org.webmproject.webp", "public.heic", "public.heif", "public.png", "public.jpeg", "public.tiff"];

    private static readonly Dictionary<string, string> DocumentTypes = new(StringComparer.Ordinal)
    {
        ["public.plain-text"] = ".txt",
        ["public.utf8-plain-text"] = ".txt",
        ["org.openxmlformats.wordprocessingml.document"] = ".docx",
        ["com.adobe.pdf"] = ".pdf",
        ["public.jpeg"] = ".jpg",
        ["public.png"] = ".png",
        ["org.webmproject.webp"] = ".webp",
        ["public.heic"] = ".heic",
        ["public.heif"] = ".heif",
    };

    private static (string Type, string Name)? DropFileRepresentation(string name, string[] types, bool hasFileUrl)
    {
        if (DesktopFile.Supports(name))
        {
            var extension = Path.GetExtension(name).ToLowerInvariant();
            if (extension == ".jpeg") extension = ".jpg";
            var exact = Array.Find(types, type => DocumentTypes.TryGetValue(type, out var suffix) && suffix == extension);
            return exact is null ? null : (exact, name);
        }

        if (hasFileUrl) return null;
        var native = Array.Find(types, type => DocumentTypes.TryGetValue(type, out var suffix) && suffix != ".txt");
        return native is null ? null : (native, name + DocumentTypes[native]);
    }

    internal static bool QueueFiles(IEnumerable<NSUrl> urls)
    {
        var inputs = new List<(string Name, Func<Task<ShareInboxItem>> Stage)>();
        var scopedUrls = new List<NSUrl>();
        foreach (var url in urls)
        {
            if (FileName(url) is not { } name) continue;
            if (url.StartAccessingSecurityScopedResource()) scopedUrls.Add(url);
            inputs.Add((name, () => StageFileAsync(url, name)));
        }
        if (inputs.Count == 0) return false;
        _ = ReceiveFileUrlsAsync(inputs, scopedUrls);
        return true;
    }

    private static async Task ReceiveFileUrlsAsync(
        List<(string Name, Func<Task<ShareInboxItem>> Stage)> inputs, List<NSUrl> scopedUrls)
    {
        // Finder can end its temporary access when the native open callback returns.
        // All URLs acquire their scope there, even if an earlier file is still copying.
        try
        {
            await DesktopImport.ReceiveBatchAsync(inputs).ConfigureAwait(false);
        }
        finally
        {
            foreach (var url in scopedUrls) url.StopAccessingSecurityScopedResource();
        }
    }

    private static string? FileName(NSUrl url) => url.IsFileUrl && url.Path is { } path
        && Path.GetFileName(path) is { } name && DesktopFile.Supports(name) ? name : null;

    private static async Task<ShareInboxItem> StageUrlAsync(NSUrl url, string name)
    {
        var scoped = url.StartAccessingSecurityScopedResource();
        try
        {
            return await StageFileAsync(url, name).ConfigureAwait(false);
        }
        finally
        {
            if (scoped) url.StopAccessingSecurityScopedResource();
        }
    }

    private static async Task<ShareInboxItem> StageFileAsync(NSUrl url, string name)
    {
        await using var stream = File.OpenRead(url.Path ?? throw new IOException("Не вдалося відкрити файл."));
        return await ShareInbox.StageAsync("file", name, stream).ConfigureAwait(false);
    }

    internal static async Task<ShareInboxItem?> StageClipboardImageAsync()
    {
        var pasteboard = UIPasteboard.General;
        var types = pasteboard.Types ?? [];
        InvalidDataException? failure = null;
        foreach (var type in ClipboardImageTypes.Where(candidate => types.Contains(candidate, StringComparer.Ordinal)))
        {
            using var data = pasteboard.DataForPasteboardType(type);
            if (data is null || data.Length > MaxClipboardImageBytes)
            {
                failure ??= new InvalidDataException(data is null
                    ? "Не вдалося прочитати зображення з буфера."
                    : "Зображення з буфера завелике.");
                continue;
            }
            (NSData? Converted, string Extension) prepared;
            try { prepared = await Task.Run(() => PrepareClipboardImage(data, type)); }
            catch (InvalidDataException error)
            {
                failure ??= error;
                continue;
            }
            using var converted = prepared.Converted;
            var image = converted ?? data;
            if (image.Length > Reader.CoreClient.CoreClient.MaxImportBytes)
            {
                failure ??= new InvalidDataException("Зображення з буфера перевищує 20 МБ.");
                continue;
            }
            using var stream = image.AsStream();
            return await ShareInbox.StageAsync("file", "Зображення з буфера" + prepared.Extension, stream);
        }
        if (failure is not null) throw failure;
        if (pasteboard.HasImages)
            throw new InvalidDataException("Формат зображення з буфера не підтримується.");
        return null;
    }

    private static (NSData? Converted, string Extension) PrepareClipboardImage(NSData data, string type)
    {
        using var pool = new NSAutoreleasePool();
        using var source = CGImageSource.FromData(data);
        if (source?.TypeIdentifier?.ToString() is not { } detected ||
            (detected != type && !(IsHeif(detected) && IsHeif(type))) ||
            source.GetStatus(0) != CGImageSourceStatus.Complete)
            throw new InvalidDataException("macOS не може прочитати зображення з буфера.");

        var properties = source.GetProperties(0, null);
        var width = properties?.PixelWidth ?? 0;
        var height = properties?.PixelHeight ?? 0;
        if (width <= 0 || height <= 0 || (long)width * height > MaxClipboardImagePixels)
            throw new InvalidDataException("Розмір зображення з буфера не підтримується.");
        if (detected != "public.tiff") return (null, DocumentTypes[detected]);

        using var image = source.CreateThumbnail(0, new CGImageThumbnailOptions
        {
            CreateThumbnailFromImageAlways = true,
            CreateThumbnailWithTransform = true,
            MaxPixelSize = 4096,
        }) ?? throw new InvalidDataException("macOS не може прочитати зображення з буфера.");
        using var png = UIImage.FromImage(image);
        return (png.AsPNG() ?? throw new InvalidDataException("Не вдалося підготувати фото з буфера."), ".png");
    }

    private static bool IsHeif(string type) => type is "public.heic" or "public.heif";

    internal static async Task<bool> QueueDropAsync(DropEventArgs args)
    {
        if (args.PlatformArgs?.DropSession is not { } session) return false;
        var staged = new List<ShareInboxItem>();
        var rejected = new List<(string Name, Exception Error)>();
        foreach (var item in session.Items)
        {
            var suggested = "Документ";
            try
            {
                var provider = item.ItemProvider;
                suggested = Path.GetFileName(provider.SuggestedName ?? suggested);
                var hasFileUrl = provider.HasItemConformingTo("public.file-url");
                if (DropFileRepresentation(suggested, provider.RegisteredTypeIdentifiers, hasFileUrl) is { } file)
                    staged.Add(await StageProviderAsync(provider, file.Type, file.Name, hasFileUrl));
                else if (hasFileUrl)
                {
                    if (await StageFileItemAsync(provider) is { } original)
                        staged.Add(original);
                    else if (DesktopFile.Supports(suggested))
                        throw new InvalidDataException("Не вдалося отримати оригінальний файл.");
                }
                else if (DesktopFile.Supports(suggested))
                    throw new InvalidDataException("Перетягування не надало файл у його оригінальному форматі.");
                else
                {
                    var address = await GetDropAddressAsync(provider);
                    var text = address is null ? await GetDropTextAsync(provider) : null;
                    if (await DesktopImport.StageDropTextAsync(address, text) is { } payload)
                        staged.Add(payload);
                }
            }
            catch (Exception error)
            {
                rejected.Add((suggested, error));
            }
        }
        // Import prompts can change the current page. Open them after all providers have been staged.
        DesktopImport.PublishBatch(staged, rejected);
        return staged.Count > 0 || rejected.Count > 0;
    }

    private static Task<ShareInboxItem?> StageFileItemAsync(NSItemProvider provider)
    {
        var completion = new TaskCompletionSource<ShareInboxItem?>(TaskCreationOptions.RunContinuationsAsynchronously);
        provider.LoadObject<NSUrl>((url, error) =>
        {
            if (error is not null) completion.TrySetException(new IOException(error.LocalizedDescription));
            else if (url is not null && FileName(url) is { } name) StageInCallback(completion, url, name);
            else completion.TrySetResult(null);
        });
        return completion.Task;
    }

    private static Task<Uri?> GetDropAddressAsync(NSItemProvider provider)
    {
        if (!provider.HasItemConformingTo("public.url")) return Task.FromResult<Uri?>(null);

        var completion = new TaskCompletionSource<Uri?>(TaskCreationOptions.RunContinuationsAsynchronously);
        provider.LoadObject<NSUrl>((url, error) =>
            completion.TrySetResult(error is null && url is not null ? WebArticles.Address(url.AbsoluteString) : null));
        return completion.Task;
    }

    private static Task<string?> GetDropTextAsync(NSItemProvider provider)
    {
        var type = provider.HasItemConformingTo("public.utf8-plain-text") ? "public.utf8-plain-text" : "public.plain-text";
        if (!provider.HasItemConformingTo(type)) return Task.FromResult<string?>(null);

        var completion = new TaskCompletionSource<string?>(TaskCreationOptions.RunContinuationsAsynchronously);
        void LoadUtf8() => provider.LoadDataRepresentation(type, (data, error) =>
        {
            completion.TrySetResult(error is null && data is not null &&
                data.Length <= Reader.CoreClient.CoreClient.MaxImportBytes
                ? Encoding.UTF8.GetString(data.ToArray()) : null);
        });
        if (provider.CanLoadObject(typeof(NSString)))
        {
            provider.LoadObject<NSString>((value, error) =>
            {
                if (error is null && value is not null)
                    completion.TrySetResult(value.ToString());
                else
                    LoadUtf8();
            });
        }
        else LoadUtf8();
        return completion.Task;
    }

    private static async Task<ShareInboxItem> StageProviderAsync(
        NSItemProvider provider, string type, string name, bool hasFileUrl)
    {
        var completion = new TaskCompletionSource<ShareInboxItem?>(TaskCreationOptions.RunContinuationsAsynchronously);
        string? unavailable = null;
        provider.LoadInPlaceFileRepresentation(type, (url, _, error) =>
        {
            if (error is not null || url is null)
            {
                unavailable = error?.LocalizedDescription ?? "Не вдалося прочитати перетягнутий файл.";
                completion.TrySetResult(null);
                return;
            }
            StageInCallback(completion, url, name);
        });
        if (await completion.Task.ConfigureAwait(false) is { } item) return item;
        // Advertising a type does not guarantee that its in-place representation is available.
        if (hasFileUrl && await StageFileItemAsync(provider).ConfigureAwait(false) is { } original) return original;
        throw new IOException(unavailable);
    }

    private static void StageInCallback(TaskCompletionSource<ShareInboxItem?> completion, NSUrl url, string name)
    {
        // A provider may invalidate its temporary URL as soon as this callback returns.
        try { completion.TrySetResult(StageUrlAsync(url, name).GetAwaiter().GetResult()); }
        catch (Exception error) { completion.TrySetException(error); }
    }
}
