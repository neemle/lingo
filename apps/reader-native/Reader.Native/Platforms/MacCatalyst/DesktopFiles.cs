using Foundation;

namespace Reader.Native.Services;

internal static class DesktopFiles
{
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

    internal static bool QueueFile(NSUrl url) => FromUrl(url) is { } file && DesktopImport.Enqueue(file);

    private static DesktopFile? FromUrl(NSUrl url)
    {
        if (!url.IsFileUrl || url.Path is not { } path) return null;
        var name = Path.GetFileName(path);
        return DesktopFile.Supports(name) ? new DesktopFile(name, () => ReadUrlAsync(url)) : null;
    }

    private static async Task<byte[]> ReadUrlAsync(NSUrl url)
    {
        var scoped = url.StartAccessingSecurityScopedResource();
        try
        {
            await using var stream = File.OpenRead(url.Path ?? throw new IOException("Не вдалося відкрити файл."));
            return await NativeFiles.ReadBoundedAsync(stream, Reader.CoreClient.CoreClient.MaxImportBytes)
                .ConfigureAwait(false);
        }
        finally
        {
            if (scoped) url.StopAccessingSecurityScopedResource();
        }
    }

    internal static async Task<int> QueueDropAsync(DropEventArgs args)
    {
        if (args.PlatformArgs?.DropSession is not { } session) return 0;
        var count = 0;
        foreach (var item in session.Items)
        {
            var provider = item.ItemProvider;
            var declaredType = provider.RegisteredTypeIdentifiers.FirstOrDefault(DocumentTypes.ContainsKey);
            if (declaredType is null && provider.HasItemConformingTo("public.file-url"))
            {
                var completion = new TaskCompletionSource<NSUrl?>(TaskCreationOptions.RunContinuationsAsynchronously);
                provider.LoadItem("public.file-url", null, (value, error) =>
                    completion.TrySetResult(error is null ? value as NSUrl : null));
                if (await completion.Task is { } original && QueueFile(original)) count++;
                continue;
            }
            if (declaredType is null) continue;
            var suggested = Path.GetFileName(provider.SuggestedName ?? "Документ");
            var name = DesktopFile.Supports(suggested) ? suggested : suggested + DocumentTypes[declaredType];
            if (DesktopImport.Enqueue(new DesktopFile(name, () => ReadProviderAsync(provider, declaredType)))) count++;
        }
        return count;
    }

    internal static async Task<Uri?> GetDropAddressAsync(DropEventArgs args)
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

    private static Task<byte[]> ReadProviderAsync(NSItemProvider provider, string type)
    {
        var completion = new TaskCompletionSource<byte[]>(TaskCreationOptions.RunContinuationsAsynchronously);
        provider.LoadInPlaceFileRepresentation(type, (url, _, error) =>
        {
            if (error is not null || url is null)
            {
                completion.TrySetException(new IOException(error?.LocalizedDescription ?? "Не вдалося прочитати перетягнутий файл."));
                return;
            }
            try
            {
                // A provider may delete its temporary file when this callback returns.
                completion.TrySetResult(ReadUrlAsync(url).GetAwaiter().GetResult());
            }
            catch (Exception failure) { completion.TrySetException(failure); }
        });
        return completion.Task;
    }
}
