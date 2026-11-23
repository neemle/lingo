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
    private static readonly ConcurrentQueue<ShareInboxItem[]> Pending = new();
    private static int receiving;
    internal static event Action? Available;
    internal static bool HasPending => !Pending.IsEmpty;
    internal static bool HasIncoming => HasPending || Volatile.Read(ref receiving) > 0;
    internal static bool TryTake(out ShareInboxItem[]? items) => Pending.TryDequeue(out items);

    internal static IDisposable TrackIncoming()
    {
        Interlocked.Increment(ref receiving);
        return new IncomingBatch();
    }

    private sealed class IncomingBatch : IDisposable
    {
        private int disposed;

        public void Dispose()
        {
            if (Interlocked.Exchange(ref disposed, 1) == 0) Interlocked.Decrement(ref receiving);
        }
    }

    internal static async Task ReceiveBatchAsync(IEnumerable<(string Name, Func<Task<ShareInboxItem>> Stage)> inputs)
    {
        using var incoming = TrackIncoming();
        var staged = new List<ShareInboxItem>();
        (string Name, Exception Error)? firstFailure = null;
        var rejected = 0;
        foreach (var (name, stage) in inputs)
        {
            try { staged.Add(await stage().ConfigureAwait(false)); }
            catch (Exception error)
            {
                firstFailure ??= (name, error);
                rejected++;
            }
        }

        if (firstFailure is { } failed)
            RecordFailure(failed.Name, failed.Error, rejected);
        PublishBatch(staged);
        if (rejected > 0 && staged.Count == 0) Available?.Invoke();
    }

    private static string? failure;

    internal static void ReportFailure(string name, Exception error)
    {
        RecordFailure(name, error, 1);
        Available?.Invoke();
    }

    private static void RecordFailure(string name, Exception error, int rejected)
    {
        var detail = $"{Path.GetFileName(name)} · {error.GetBaseException().Message}";
        Interlocked.Exchange(ref failure, rejected == 1 ? detail : $"Не збережено файлів: {rejected}. {detail}");
    }

    internal static string? TakeFailure() => Interlocked.Exchange(ref failure, null);

    internal static void PublishBatch(IEnumerable<ShareInboxItem> items)
    {
        var batch = items.ToArray();
        if (batch.Length == 0) return;
        Pending.Enqueue(batch);
        Available?.Invoke();
    }

    internal static async Task<bool> EnqueueDropTextAsync(Uri? address, string? text)
    {
        if (await StageDropTextAsync(address, text).ConfigureAwait(false) is not { } item) return false;
        PublishBatch([item]);
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
