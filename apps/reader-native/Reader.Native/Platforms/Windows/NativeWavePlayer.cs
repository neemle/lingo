using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Storage.Streams;

namespace Reader.Native.Services;

internal sealed class NativeWavePlayer
{
    private MediaPlayer? active;

    internal bool Pause()
    {
        if (active is null) return false;
        active.Pause();
        return true;
    }

    internal bool Resume()
    {
        if (active is null) return false;
        active.Play();
        return true;
    }

    internal async Task PlayAsync(byte[] wav, Action? started, CancellationToken token)
    {
        using var stream = new InMemoryRandomAccessStream();
        using (var writer = new DataWriter(stream))
        {
            writer.WriteBytes(wav);
            await writer.StoreAsync().AsTask(token);
            writer.DetachStream();
        }
        stream.Seek(0);
        using var source = MediaSource.CreateFromStream(stream, "audio/wav");
        using var player = new MediaPlayer();
        var completed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        player.MediaEnded += (_, _) => completed.TrySetResult();
        player.MediaFailed += (_, args) =>
            completed.TrySetException(new InvalidOperationException(args.ErrorMessage));
        using var registration = token.Register(() => completed.TrySetCanceled(token));
        try
        {
            token.ThrowIfCancellationRequested();
            active = player;
            player.Source = source;
            player.Play();
            started?.Invoke();
            await completed.Task;
        }
        finally
        {
            active = null;
            player.Source = null;
        }
    }
}
