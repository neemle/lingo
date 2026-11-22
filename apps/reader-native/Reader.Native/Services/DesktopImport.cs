using System.Collections.Concurrent;

namespace Reader.Native.Services;

internal sealed record DesktopFile(string Name, Func<Task<byte[]>> Read)
{
    internal static bool Supports(string name) => Path.GetExtension(name).ToLowerInvariant() is
        ".txt" or ".docx" or ".pdf" or ".jpg" or ".jpeg" or ".png" or ".webp" or ".heic" or ".heif";
}

internal static class DesktopImport
{
    internal static readonly SemaphoreSlim Gate = new(1, 1);
    private static readonly ConcurrentQueue<DesktopFile> Pending = new();
    internal static event Action? Available;
    internal static bool HasPending => !Pending.IsEmpty;
    internal static bool TryTake(out DesktopFile? file) => Pending.TryDequeue(out file);

    internal static bool Enqueue(DesktopFile file)
    {
        if (!DesktopFile.Supports(file.Name)) return false;
        Pending.Enqueue(file);
        Available?.Invoke();
        return true;
    }
}
