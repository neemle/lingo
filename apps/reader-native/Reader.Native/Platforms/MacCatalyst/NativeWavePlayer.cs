using AVFoundation;
using Foundation;

namespace Reader.Native.Services;

internal sealed class NativeWavePlayer
{
    private AVAudioPlayer? active;

    internal TimeSpan? Position => active is { } player ? TimeSpan.FromSeconds(player.CurrentTime) : null;

    internal bool Pause()
    {
        if (active?.Playing != true) return false;
        active.Pause();
        return true;
    }

    internal bool Resume() => active?.Play() == true;

    internal async Task PlayAsync(byte[] wav, float rate, Action? started, CancellationToken token)
    {
        using var data = NSData.FromArray(wav);
        using var player = AVAudioPlayer.FromData(data, out var error);
        using (error)
        {
            if (player is null)
                throw new InvalidOperationException(error?.LocalizedDescription ?? "Не вдалося відкрити аудіо.");
        }

        var completed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        player.FinishedPlaying += (_, args) =>
        {
            if (args.Status) completed.TrySetResult();
            else completed.TrySetException(new InvalidOperationException("Не вдалося відтворити аудіо."));
        };
        player.DecoderError += (_, _) =>
            completed.TrySetException(new InvalidOperationException("Не вдалося декодувати аудіо."));
        using var registration = token.Register(() => completed.TrySetCanceled(token));
        try
        {
            token.ThrowIfCancellationRequested();
            player.EnableRate = rate != 1;
            if (player.EnableRate) player.Rate = rate;
            active = player;
            if (!player.Play()) throw new InvalidOperationException("Не вдалося запустити аудіо.");
            started?.Invoke();
            await completed.Task;
        }
        finally
        {
            active = null;
            player.Stop();
        }
    }
}
