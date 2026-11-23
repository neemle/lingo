using System.Text.Json.Serialization;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Native.Services;

internal sealed record ShareInboxItem(Guid Id, string Kind, string Name, DateTimeOffset ReceivedAt, long Bytes,
    [property: JsonPropertyName("photoLanguage")] string? SourceLanguage = null, bool ScanPdf = false)
{
    internal string Label => $"{Name} · {ReceivedAt.LocalDateTime:g}";
}

internal sealed class ShareInboxFullException() : InvalidOperationException(
    "Вхідні матеріали заповнено. Завершіть або видаліть попередні імпорти в бібліотеці.");

internal static class ShareInbox
{
    private const int MaxItems = 16;
    private const int MaxMetadataBytes = 4096;
    private static readonly SemaphoreSlim Staging = new(1, 1);
    private static readonly Lazy<string> Storage = new(Initialize);

    internal static bool TryPauseStaging() => Staging.Wait(0);

    internal static void ResumeStaging() => Staging.Release();

    internal static async Task<ShareInboxItem> StageAsync(string kind, string name, Stream source, bool scanPdf = false,
        CancellationToken cancellation = default, IProgress<long>? progress = null)
    {
        var id = Guid.NewGuid();
        name = SafeName(name);
        var maximum = Limit(kind, name);
        await Staging.WaitAsync(cancellation).ConfigureAwait(false);
        try
        {
            if (EntryFolders().Take(MaxItems).Count() == MaxItems)
                throw new ShareInboxFullException();
            var temporary = Path.Combine(Storage.Value, $".incoming-{id:N}");
            Directory.CreateDirectory(temporary);
            try
            {
                if (source.CanSeek && source.Length - source.Position > maximum)
                    throw new InvalidDataException($"Максимальний розмір файлу — {maximum / (1024 * 1024d):G} МіБ.");
                long length;
                await using (var target = new FileStream(Path.Combine(temporary, "payload"),
                    FileMode.CreateNew, FileAccess.Write, FileShare.None, 64 * 1024, FileOptions.Asynchronous))
                {
                    await NativeFiles.CopyBoundedAsync(source, target, maximum, cancellation, progress).ConfigureAwait(false);
                    cancellation.ThrowIfCancellationRequested();
                    length = target.Length;
                    if (length == 0) throw new InvalidDataException("Вхідний матеріал порожній.");
                    target.Flush(flushToDisk: true);
                }
                cancellation.ThrowIfCancellationRequested();
                var item = new ShareInboxItem(id, kind, name, DateTimeOffset.UtcNow, length, ScanPdf: scanPdf);
                WriteMetadata(Path.Combine(temporary, "item.json"), item);
                Directory.Move(temporary, Folder(id));
                return item;
            }
            finally { DeleteTemporary(temporary); }
        }
        finally { Staging.Release(); }
    }

    internal static ShareInboxItem[] List() => EntryFolders().Select(ReadItem)
        .OrderBy(item => item.ReceivedAt).ToArray();

    internal static async Task<byte[]> ReadAsync(ShareInboxItem item)
    {
        await using var source = File.OpenRead(PayloadPath(item));
        var maximum = (int)Math.Min(Limit(item.Kind, item.Name), Client.MaxImportBytes);
        var bytes = await NativeFiles.ReadBoundedAsync(source, maximum).ConfigureAwait(false);
        if (bytes.LongLength != item.Bytes) throw new InvalidDataException("Локальна копія пошкоджена.");
        return bytes;
    }

    internal static string PayloadPath(ShareInboxItem item)
    {
        var path = Path.Combine(Folder(item.Id), "payload");
        var file = new FileInfo(path);
        if (!file.Exists || file.Attributes.HasFlag(FileAttributes.ReparsePoint) ||
            file.Length != item.Bytes || file.Length <= 0 || file.Length > Limit(item.Kind, item.Name))
            throw new InvalidDataException("Локальна копія пошкоджена.");
        return path;
    }

    internal static bool Contains(ShareInboxItem item) => Directory.Exists(Folder(item.Id));

    internal static void SaveSourceLanguage(ShareInboxItem item, string language)
    {
        var folder = Folder(item.Id);
        var replacement = Path.Combine(folder, "item.pending.json");
        WriteMetadata(replacement, item with { SourceLanguage = language });
        File.Move(replacement, Path.Combine(folder, "item.json"), overwrite: true);
    }

    internal static void Remove(ShareInboxItem item)
    {
        var folder = Folder(item.Id);
        if (!Directory.Exists(folder)) return;
        var retired = Path.Combine(Storage.Value, $".retired-{item.Id:N}");
        Directory.Move(folder, retired);
        DeleteTemporary(retired);
    }

    private static string Initialize()
    {
        var root = Path.Combine(FileSystem.AppDataDirectory, "share-inbox");
        CreateStorage(root);
        foreach (var folder in Directory.EnumerateDirectories(root))
        {
            var name = Path.GetFileName(folder);
            if (name.StartsWith(".retired-", StringComparison.Ordinal) ||
                name.StartsWith(".incoming-", StringComparison.Ordinal)
                && Directory.GetLastWriteTimeUtc(folder) < DateTime.UtcNow.AddDays(-1))
                DeleteTemporary(folder);
        }
        return root;
    }

    private static IEnumerable<string> EntryFolders()
    {
        var root = Storage.Value;
        // Reset can remove the directory while the cached path remains valid.
        CreateStorage(root);
        return Directory.EnumerateDirectories(root).Where(path => Guid.TryParseExact(Path.GetFileName(path), "N", out _));
    }

    private static void CreateStorage(string path)
    {
        if (OperatingSystem.IsWindows()) Directory.CreateDirectory(path);
        else Directory.CreateDirectory(path, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
    }

    private static string Folder(Guid id) => Path.Combine(Storage.Value, id.ToString("N"));

    private static void WriteMetadata(string path, ShareInboxItem item)
    {
        using var metadata = new FileStream(path, FileMode.Create);
        metadata.Write(NativeFiles.Encode(item));
        metadata.Flush(flushToDisk: true);
    }

    private static ShareInboxItem ReadItem(string folder)
    {
        var id = Guid.ParseExact(Path.GetFileName(folder), "N");
        try
        {
            using var source = new FileStream(Path.Combine(folder, "item.json"), FileMode.Open,
                FileAccess.Read, FileShare.Read | FileShare.Delete);
            using var bytes = new MemoryStream();
            NativeFiles.CopyBoundedAsync(source, bytes, MaxMetadataBytes).GetAwaiter().GetResult();
            var item = NativeFiles.Decode<ShareInboxItem>(bytes.ToArray());
            if (item.Id == id && item.Bytes > 0 && !string.IsNullOrEmpty(item.Name) && item.Bytes <= Limit(item.Kind, item.Name))
                return item;
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or System.Text.Json.JsonException) { }
        return new ShareInboxItem(id, "damaged", "Пошкоджений вхідний матеріал", Directory.GetCreationTimeUtc(folder), 0);
    }

    private static long Limit(string kind, string name) => kind switch
    {
#if MACCATALYST
        "file" when Path.GetExtension(name).Equals(".pdf", StringComparison.OrdinalIgnoreCase) => Client.MaxPdfBytes,
#endif
        "file" or "text" => Client.MaxImportBytes,
        "url" => 16 * 1024,
        _ => throw new InvalidDataException("Невідомий тип вхідного матеріалу."),
    };

    private static string SafeName(string name)
    {
        var leaf = Path.GetFileName(name.Replace('\\', '/'));
        var extension = Path.GetExtension(leaf);
        var title = string.Concat(Path.GetFileNameWithoutExtension(leaf).Where(character => !char.IsControl(character))).Trim();
        if (title.Length > 180)
        {
            var length = char.IsHighSurrogate(title[179]) ? 179 : 180;
            title = title[..length];
        }
        return (title.Length == 0 ? "Документ" : title) + extension;
    }

    private static void DeleteTemporary(string path)
    {
        try { if (Directory.Exists(path)) Directory.Delete(path, recursive: true); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
    }
}
