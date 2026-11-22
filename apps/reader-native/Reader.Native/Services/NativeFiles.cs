using System.Text.Json;

namespace Reader.Native.Services;

public static class NativeFiles
{
    public static async Task<byte[]> ReadBoundedAsync(FileResult file, int maximum)
    {
        await using var stream = await file.OpenReadAsync();
        return await ReadBoundedAsync(stream, maximum);
    }

    internal static async Task<byte[]> ReadBoundedAsync(Stream stream, int maximum)
    {
        using var copy = new MemoryStream();
        var buffer = new byte[64 * 1024];
        int count;
        while ((count = await stream.ReadAsync(buffer).ConfigureAwait(false)) != 0)
        {
            if (copy.Length + count > maximum) throw new InvalidDataException("Файл перевищує допустимий розмір.");
            await copy.WriteAsync(buffer.AsMemory(0, count)).ConfigureAwait(false);
        }
        return copy.ToArray();
    }

    public static async Task ShareBytesAsync(string name, byte[] bytes)
    {
        var directory = Path.Combine(FileSystem.CacheDirectory, "exports", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var path = Path.Combine(directory, Path.GetFileName(name));
        await File.WriteAllBytesAsync(path, bytes);
        await Share.Default.RequestAsync(new ShareFileRequest("Зберегти файл", new ShareFile(path)));
    }

    public static byte[] Encode<T>(T value) => JsonSerializer.SerializeToUtf8Bytes(value,
        new JsonSerializerOptions(JsonSerializerDefaults.Web));
    public static T Decode<T>(byte[] bytes) => JsonSerializer.Deserialize<T>(bytes,
        new JsonSerializerOptions(JsonSerializerDefaults.Web)) ?? throw new InvalidDataException("Порожній архів");
}
