using Reader.Presentation;

namespace Reader.Native.Services;

public sealed class LocalSpeech : IDisposable
{
    internal const string CompletedStatus = "Озвучення завершено.";

    private readonly SpeechPlayback playback = new();
    private readonly NativeSpeech speech = new();
    public float Rate { get; set; } = 1;

    public Task<string?> SpeakAsync(string text, string language, Action<int, int>? boundary = null)
    {
        var rate = Rate;
        return playback.PlayAsync(async token =>
        {
            await speech.SpeakAsync(text, language, rate, boundary, token);
            return CompletedStatus;
        });
    }

    public bool Pause() => speech.Pause();
    public bool Resume() => speech.Resume();
    public void Stop() => playback.Stop();
    public void Dispose() => Stop();
}
