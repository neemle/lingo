using System.Text.Json;

namespace Reader.Native.Services;

public static class NativeFiles
{
    private static readonly SemaphoreSlim ExportDialog = new(1, 1);

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

    internal static async Task CopyBoundedAsync(Stream stream, Stream target, int maximum,
        CancellationToken cancellation = default)
    {
        var buffer = new byte[64 * 1024];
        long total = 0;
        int count;
        while ((count = await stream.ReadAsync(buffer.AsMemory(), cancellation).ConfigureAwait(false)) != 0)
        {
            if ((total += count) > maximum) throw new InvalidDataException("Файл перевищує допустимий розмір.");
            await target.WriteAsync(buffer.AsMemory(0, count), cancellation).ConfigureAwait(false);
        }
    }

    internal static async Task<string?> ExportBytesAsync(Page owner, string name, byte[] bytes, bool share = false)
    {
        if (!ExportDialog.Wait(0)) return null;
        try
        {
            if (share)
            {
                await ShareBytesAsync(name, bytes);
                return "Відкрито меню поширення.";
            }
            var window = owner.Window ?? throw new InvalidOperationException("Вікно застосунку закрито.");
            return await NativeFileSave.SaveAsync(window, name, bytes) ? "Файл збережено." : null;
        }
        finally { ExportDialog.Release(); }
    }

    private static async Task ShareBytesAsync(string name, byte[] bytes)
    {
        var directory = Path.Combine(FileSystem.CacheDirectory, "exports", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var path = Path.Combine(directory, Path.GetFileName(name));
        await File.WriteAllBytesAsync(path, bytes);
        await Share.Default.RequestAsync(new ShareFileRequest("Поширити файл", new ShareFile(path)));
    }

    public static byte[] Encode<T>(T value) => JsonSerializer.SerializeToUtf8Bytes(value,
        new JsonSerializerOptions(JsonSerializerDefaults.Web));
    public static T Decode<T>(byte[] bytes) => JsonSerializer.Deserialize<T>(bytes,
        new JsonSerializerOptions(JsonSerializerDefaults.Web)) ?? throw new InvalidDataException("Порожній архів");
}
