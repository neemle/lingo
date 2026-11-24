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
    internal sealed record Batch(ShareInboxItem[] Items, string? Failure);

    internal static readonly SemaphoreSlim Gate = new(1, 1);
    private static readonly ConcurrentQueue<Batch> Pending = new();
    private static int receiving;
    internal static event Action? Available;
    internal static bool HasPending => !Pending.IsEmpty;
    internal static bool HasIncoming => HasPending || Volatile.Read(ref receiving) > 0;
    internal static bool TryPeek(out Batch? batch) => Pending.TryPeek(out batch);
    internal static bool TryTake(out Batch? batch) => Pending.TryDequeue(out batch);

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
        var rejected = new List<(string Name, Exception Error)>();
        foreach (var (name, stage) in inputs)
        {
            try { staged.Add(await stage().ConfigureAwait(false)); }
            catch (Exception error) { rejected.Add((name, error)); }
        }

        PublishBatch(staged, rejected);
    }

    internal static void PublishBatch(IEnumerable<ShareInboxItem> items,
        IReadOnlyList<(string Name, Exception Error)>? rejected = null)
    {
        var staged = items.ToArray();
        string? failure = null;
        if (rejected is { Count: > 0 })
        {
            var (name, error) = rejected[0];
            var detail = $"{Path.GetFileName(name)} · {error.GetBaseException().Message}";
            failure = rejected.Count == 1 ? detail : $"Не збережено файлів: {rejected.Count}. {detail}";
        }
        if (staged.Length == 0 && failure is null) return;
        Pending.Enqueue(new Batch(staged, failure));
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
