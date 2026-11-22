using Reader.Contracts;
using Reader.Presentation;

namespace Reader.Native.Services;

public sealed class LocalSpeech : IDisposable
{
    internal const string CompletedStatus = "Озвучення завершено.";

    private readonly SpeechPlayback playback = new();
    private readonly NativeSpeech speech = new();
    private readonly NativeWavePlayer wave = new();
    public float Rate { get; set; } = 1;
    internal string? PreferredVoice { get; set; }

    internal static SpeechVoice[] VoicesForLanguage(string language) =>
        SpeechVoice.ForLanguage(NativeSpeech.InstalledVoices(), language);

    public Task<string?> SpeakAsync(string text, string language, Action<int, int>? boundary = null)
    {
        var rate = Rate;
        var voice = PreferredVoice;
        return playback.PlayAsync(async token =>
        {
            await speech.SpeakAsync(text, language, voice, rate, boundary, token);
            return CompletedStatus;
        });
    }

    internal Task<string?> PlayWaveAsync(SpeechAudio audio, Action? started) => playback.PlayAsync(async token =>
    {
        if (audio.SampleRate is <= 0 or > 48_000 || audio.Samples <= 0 ||
            audio.Samples > audio.SampleRate * 45 || audio.WavBytes.Length != 44 + audio.Samples * 2)
            throw new InvalidOperationException("Голосовий пакет повернув некоректне аудіо.");
        var wav = Array.ConvertAll(audio.WavBytes, value => checked((byte)value));
        await wave.PlayAsync(wav, started, token);
        return CompletedStatus;
    });

    public bool Pause() => wave.Pause() || speech.Pause();
    public bool Resume() => wave.Resume() || speech.Resume();
    public void Stop() => playback.Stop();
    public void Dispose() => Stop();
}
