using System.Buffers.Binary;
using Reader.Contracts;
using Reader.Presentation;

namespace Reader.Native.Services;

public sealed class LocalSpeech : IDisposable
{
    internal const int MaxModelAudioSeconds = 75;
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
            var input = SourceRanges.PrepareSpeech(text, boundary);
            await speech.SpeakAsync(input.Text, language, voice, rate, input.Boundary, token);
            return CompletedStatus;
        });
    }

    internal TimeSpan? WavePosition => wave.Position;

    internal Task<string?> PlayWaveAsync(SpeechAudio audio, int startAtMs, Action<int> started) => playback.PlayAsync(async token =>
    {
        if (audio.SampleRate is <= 0 or > 48_000 || audio.Samples <= 0 ||
            !float.IsFinite(audio.PlaybackRate) || audio.PlaybackRate is < 0.5f or > 2 ||
            audio.Samples > audio.SampleRate * MaxModelAudioSeconds || audio.WavBytes.Length != 44 + audio.Samples * 2)
            throw new InvalidOperationException("Голосовий пакет повернув некоректне аудіо.");
        // Both packaged voice engines return a 44-byte mono PCM header and 16-bit samples.
        // Trim on a sample boundary so playback starts at the same position on both platforms.
        var firstSample = (int)((long)startAtMs * audio.SampleRate / 1000);
        if (firstSample < 0 || firstSample >= audio.Samples)
        {
            firstSample = 0;
            startAtMs = 0;
        }
        var payloadBytes = (audio.Samples - firstSample) * 2;
        var wav = new byte[44 + payloadBytes];
        for (var index = 0; index < 44; index++)
            wav[index] = checked((byte)audio.WavBytes[index]);
        for (var index = 0; index < payloadBytes; index++)
            wav[44 + index] = checked((byte)audio.WavBytes[44 + firstSample * 2 + index]);
        BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(4, 4), wav.Length - 8);
        BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(40, 4), payloadBytes);
        await wave.PlayAsync(wav, audio.PlaybackRate, () => started(startAtMs), token);
        return CompletedStatus;
    });

    public bool Pause() => wave.Pause() || speech.Pause();
    public bool Resume() => wave.Resume() || speech.Resume();
    public void Stop() => playback.Stop();
    public void Dispose() => Stop();
}
