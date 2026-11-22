using System.Collections.Concurrent;
using System.Text;

namespace Reader.Native.Services;

internal static class DesktopFile
{
    internal static bool Supports(string name) => Path.GetExtension(name).ToLowerInvariant() is
        ".txt" or ".docx" or ".pdf" or ".jpg" or ".jpeg" or ".png" or ".webp" or ".heic" or ".heif";
}

internal static class DesktopImport
{
    internal static readonly SemaphoreSlim Gate = new(1, 1);
    private static readonly ConcurrentQueue<ShareInboxItem> Pending = new();
    internal static event Action? Available;
    internal static bool HasPending => !Pending.IsEmpty;
    internal static bool TryTake(out ShareInboxItem? item) => Pending.TryDequeue(out item);

    internal static async Task ReceiveAsync(string name, Func<Task<ShareInboxItem>> stage)
    {
        try { Publish(await stage().ConfigureAwait(false)); }
        catch (Exception error) { ReportFailure(name, error); }
    }

    private static string? failure;

    internal static void ReportFailure(string name, Exception error)
    {
        Interlocked.Exchange(ref failure, $"{Path.GetFileName(name)} · {error.GetBaseException().Message}");
        Available?.Invoke();
    }

    internal static string? TakeFailure() => Interlocked.Exchange(ref failure, null);

    internal static void Publish(ShareInboxItem item)
    {
        Pending.Enqueue(item);
        Available?.Invoke();
    }

    internal static async Task<bool> EnqueueDropTextAsync(Uri? address, string? text)
    {
        if (await StageDropTextAsync(address, text).ConfigureAwait(false) is not { } item) return false;
        Publish(item);
        return true;
    }

    internal static async Task<ShareInboxItem?> StageDropTextAsync(Uri? address, string? text)
    {
        var webAddress = address ?? WebArticles.Address(text);
        var value = webAddress?.AbsoluteUri ?? text;
        if (string.IsNullOrWhiteSpace(value)) return null;
        if (Encoding.UTF8.GetByteCount(value) > CoreClient.CoreClient.MaxImportBytes)
            throw new InvalidDataException("Текст перевищує 20 МБ.");
        using var source = new MemoryStream(Encoding.UTF8.GetBytes(value));
        return webAddress is null
            ? await ShareInbox.StageAsync("text", "Перетягнутий текст", source).ConfigureAwait(false)
            : await ShareInbox.StageAsync("url", "Вебпосилання", source).ConfigureAwait(false);
    }
}
