using System.Text.Json;

namespace Reader.Native.Services;

public static class NativeFiles
{
    private static readonly SemaphoreSlim ExportDialog = new(1, 1);
    private static readonly TimeSpan SharedExportLifetime = TimeSpan.FromDays(7);
    internal static string ExportRoot => Path.Combine(FileSystem.CacheDirectory, "exports");

    internal static bool TryPauseExports() => ExportDialog.Wait(0);

    internal static void ResumeExports() => ExportDialog.Release();

    internal static void ClearPausedExports()
    {
        if (Directory.Exists(ExportRoot)) Directory.Delete(ExportRoot, recursive: true);
    }

    public static async Task<byte[]> ReadBoundedAsync(FileResult file, int maximum)
    {
        await using var stream = await file.OpenReadAsync();
        return await ReadBoundedAsync(stream, maximum);
    }

    internal static async Task<byte[]> ReadBoundedAsync(Stream stream, int maximum, CancellationToken cancellation = default)
    {
        using var copy = new MemoryStream();
        await CopyBoundedAsync(stream, copy, maximum, cancellation).ConfigureAwait(false);
        return copy.ToArray();
    }

    internal static async Task CopyBoundedAsync(Stream stream, Stream target, long maximum,
        CancellationToken cancellation = default, IProgress<long>? progress = null)
    {
        var buffer = new byte[64 * 1024];
        long total = 0;
        long reported = 0;
        long nextReport = 0;
        int count;
        while ((count = await stream.ReadAsync(buffer.AsMemory(), cancellation).ConfigureAwait(false)) != 0)
        {
            if ((total += count) > maximum) throw new InvalidDataException("Файл перевищує допустимий розмір.");
            await target.WriteAsync(buffer.AsMemory(0, count), cancellation).ConfigureAwait(false);
            if (progress is not null && total - reported >= 1024 * 1024 && Environment.TickCount64 >= nextReport)
            {
                progress.Report(total);
                reported = total;
                nextReport = Environment.TickCount64 + 125;
            }
        }
        if (reported != total) progress?.Report(total);
    }

    internal static Task<string?> ExportBytesAsync(Page owner, string name, byte[] bytes, bool share = false) =>
        ExportAsync(owner, name, share, path => File.WriteAllBytesAsync(path, bytes));

    internal static Task<string?> ExportAsync(Page owner, string name, bool share, Func<string, Task> write) =>
        WithStagedFileAsync(name, write, retain: share, async path =>
        {
            if (share)
            {
                await Share.Default.RequestAsync(new ShareFileRequest("Поширити файл", new ShareFile(path)));
                return "Відкрито меню поширення.";
            }
            var window = owner.Window ?? throw new InvalidOperationException("Вікно застосунку закрито.");
            return await NativeFileSave.SaveAsync(window, path) ? "Файл збережено." : null;
        });

    internal static Task PreviewBytesAsync(Page owner, string name, byte[] bytes)
    {
        Task Write(string path) => File.WriteAllBytesAsync(path, bytes);
#if MACCATALYST
        return WithStagedFileAsync(name, Write, retain: false, async path =>
        {
            var window = owner.Window ?? throw new InvalidOperationException("Вікно застосунку закрито.");
            await NativeFilePreview.ShowAsync(window, path);
            return null;
        });
#else
        return WithStagedFileAsync(name, Write, retain: true, async path =>
        {
            if (!await Launcher.Default.OpenAsync(new OpenFileRequest("Оригінал", new ReadOnlyFile(path))))
                throw new InvalidOperationException("Не вдалося відкрити оригінальний файл.");
            return null;
        });
#endif
    }

    private static async Task<string?> WithStagedFileAsync(string name, Func<string, Task> write, bool retain,
        Func<string, Task<string?>> useFile)
    {
        if (!ExportDialog.Wait(0)) return null;
        DirectoryInfo? staging = null;
        var keep = false;
        try
        {
            staging = Directory.CreateDirectory(Path.Combine(ExportRoot, Guid.NewGuid().ToString("N")));
            var path = Path.Combine(staging.FullName, Path.GetFileName(name));
            await write(path);
            var result = await useFile(path);
            keep = retain;
            return result;
        }
        finally
        {
            try
            {
                if (!keep) staging?.Delete(recursive: true);
            }
            finally { ExportDialog.Release(); }
        }
    }

    internal static void PruneExpiredExports()
    {
        if (!ExportDialog.Wait(0)) return;
        DirectoryInfo[] expired;
        try
        {
            var root = new DirectoryInfo(ExportRoot);
            if (!root.Exists || root.Attributes.HasFlag(FileAttributes.ReparsePoint)) return;

            // Share receivers can open the staged file after the system dialog returns.
            var cutoff = DateTime.UtcNow - SharedExportLifetime;
            expired = root.EnumerateDirectories().Where(directory =>
                Guid.TryParseExact(directory.Name, "N", out _) &&
                !directory.Attributes.HasFlag(FileAttributes.ReparsePoint) && directory.LastWriteTimeUtc < cutoff)
                .ToArray();
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { return; }
        finally { ExportDialog.Release(); }

        foreach (var directory in expired)
        {
            try { directory.Delete(recursive: true); }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
        }
    }

    public static byte[] Encode<T>(T value) => JsonSerializer.SerializeToUtf8Bytes(value,
        new JsonSerializerOptions(JsonSerializerDefaults.Web));
    public static T Decode<T>(byte[] bytes) => JsonSerializer.Deserialize<T>(bytes,
        new JsonSerializerOptions(JsonSerializerDefaults.Web)) ?? throw new InvalidDataException("Порожній архів");
}
