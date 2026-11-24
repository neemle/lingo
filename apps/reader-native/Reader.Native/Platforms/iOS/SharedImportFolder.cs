using Foundation;

namespace Reader.Native.Services;

internal static class SharedImportFolder
{
    internal const int NameLimit = 1024;
    internal const string Payload = "payload";
    internal const string Name = "name";

    internal static string? GetPath()
    {
        using var container = NSFileManager.DefaultManager.GetContainerUrl("group.dev.lingoreader.native");
        return container?.Path is { } path ? Path.Combine(path, "incoming") : null;
    }

    internal static long Capacity(string name) => Path.GetExtension(name).ToLowerInvariant() switch
    {
        ".pdf" => 4L * 1024 * 1024 * 1024,
        ".jpg" or ".jpeg" or ".png" or ".webp" or ".heic" or ".heif" => 20L * 1024 * 1024,
        _ => 0,
    };
}
