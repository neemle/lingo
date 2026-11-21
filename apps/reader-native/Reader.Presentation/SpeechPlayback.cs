namespace Reader.Presentation;

public sealed class SpeechPlayback : IDisposable
{
    private static readonly SemaphoreSlim Audio = new(1, 1);
    private readonly object sync = new();
    private CancellationTokenSource? active;

    public async Task<string?> PlayAsync(Func<CancellationToken, Task<string>> speak)
    {
        var owner = Begin();
        var entered = false;
        try
        {
            await Audio.WaitAsync(owner.Token);
            entered = true;
            owner.Token.ThrowIfCancellationRequested();
            var message = await speak(owner.Token);
            return IsCurrent(owner) ? message : null;
        }
        catch (OperationCanceledException) { return null; }
        catch (Exception error) { return IsCurrent(owner) ? $"Озвучення недоступне: {error.Message}" : null; }
        finally
        {
            if (entered) Audio.Release();
            Finish(owner);
        }
    }

    private CancellationTokenSource Begin()
    {
        var owner = new CancellationTokenSource();
        CancellationTokenSource? previous;
        lock (sync) { previous = active; active = owner; }
        Cancel(previous);
        return owner;
    }

    private bool IsCurrent(CancellationTokenSource owner)
    {
        lock (sync) return ReferenceEquals(active, owner) && !owner.IsCancellationRequested;
    }

    private void Finish(CancellationTokenSource owner)
    {
        lock (sync) { if (ReferenceEquals(active, owner)) active = null; }
        owner.Dispose();
    }

    public void Stop()
    {
        CancellationTokenSource? previous;
        lock (sync) { previous = active; active = null; }
        Cancel(previous);
    }

    private static void Cancel(CancellationTokenSource? source)
    {
        try { source?.Cancel(); }
        catch (ObjectDisposedException)
        {
            // The completed request can release its source after Stop detaches it.
        }
    }

    public void Dispose() => Stop();
}
