using Foundation;

namespace Reader.Native.Services;

internal static class SharedImportFolder
{
    internal const int NameLimit = 1024;
    internal const int MaxItems = 16;
    internal const string Payload = "payload";
    internal const string Name = "name";
    internal const string Kind = "kind";
    internal const string BatchPrefix = "batch-";
    internal const string WordDocumentType = "org.openxmlformats.wordprocessingml.document";

    internal static string? GetPath()
    {
        using var container = NSFileManager.DefaultManager.GetContainerUrl("group.dev.lingoreader.native");
        return container?.Path is { } path ? Path.Combine(path, "incoming") : null;
    }

    internal static long Capacity(string name, string kind = "file") => kind switch
    {
        "file" => Path.GetExtension(name).ToLowerInvariant() switch
        {
            ".pdf" => 4L * 1024 * 1024 * 1024,
            ".txt" or ".docx" or ".jpg" or ".jpeg" or ".png" or ".webp" or ".heic" or ".heif" => 20L * 1024 * 1024,
            _ => 0,
        },
        "text" => 20L * 1024 * 1024,
        "url" => 16 * 1024,
        _ => 0,
    };

    internal static async Task<FileStream> LockPublicationAsync(string root, CancellationToken cancellation = default)
    {
        Directory.CreateDirectory(root);
        var path = Path.Combine(root, ".publish");
        for (var attempt = 0; ; attempt++)
        {
            cancellation.ThrowIfCancellationRequested();
            try { return File.Open(path, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None); }
            catch (IOException) when (attempt < 9)
            {
                await Task.Delay(100, cancellation).ConfigureAwait(false);
            }
        }
    }

    internal static async Task ClearPublishedAsync(string root)
    {
        if (!Directory.Exists(root)) return;
        using var publication = await LockPublicationAsync(root).ConfigureAwait(false);
        // Hidden staging is owned by the extension until it publishes a complete batch.
        foreach (var path in ExistingDirectories(root))
        {
            var name = Path.GetFileName(path);
            if (Guid.TryParseExact(name, "N", out _) ||
                name.StartsWith(BatchPrefix, StringComparison.Ordinal) &&
                Guid.TryParseExact(name.AsSpan(BatchPrefix.Length), "N", out _))
                Directory.Delete(path, recursive: IsQueueDirectory(path));
        }
    }

    internal static IEnumerable<(string Path, Guid Id, string? Batch)> QueuedFiles(string root)
    {
        foreach (var path in ExistingDirectories(root))
        {
            if (!IsQueueDirectory(path)) continue;
            var name = Path.GetFileName(path);
            if (Guid.TryParseExact(name, "N", out var id)) yield return (path, id, null);
            else if (name.StartsWith(BatchPrefix, StringComparison.Ordinal) &&
                Guid.TryParseExact(name.AsSpan(BatchPrefix.Length), "N", out _))
            {
                foreach (var file in ExistingDirectories(path))
                    if (Guid.TryParseExact(Path.GetFileName(file), "N", out var fileId) && IsQueueDirectory(file))
                        yield return (file, fileId, path);
            }
        }
    }

    private static string[] ExistingDirectories(string path)
    {
        try { return Directory.GetDirectories(path); }
        catch (DirectoryNotFoundException) { return []; }
    }

    private static bool IsQueueDirectory(string path)
    {
        try { return !File.GetAttributes(path).HasFlag(FileAttributes.ReparsePoint); }
        catch (FileNotFoundException) { return false; }
        catch (DirectoryNotFoundException) { return false; }
    }
}
