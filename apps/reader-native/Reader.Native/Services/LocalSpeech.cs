using System.Buffers.Binary;
using System.Text;
using Reader.Contracts;
using Reader.CoreClient;
using Reader.Presentation;

namespace Reader.Native.Services;

internal sealed record VoiceChoice(string Id, string Label, string? Engine = null, uint Speaker = 0, string? Revision = null);

public sealed class LocalSpeech : IDisposable
{
    internal const string SystemDefaultVoice = "@system";
    internal const int MaxModelAudioSeconds = 75;
    internal const int DefaultRatePercent = 40;
    internal const string CompletedStatus = "Озвучення завершено.";

    private readonly SpeechPlayback playback = new();
    private readonly NativeSpeech speech = new();
    private readonly NativeWavePlayer wave = new();
    public float Rate { get; set; } = 1;
    internal string? PreferredVoice { get; set; }

    private static readonly object VoiceScanGate = new();
    private static Task<SpeechVoice[]>? voiceScan;

    internal static Task<SpeechVoice[]> ReadSystemVoicesAsync(bool refresh = true, CancellationToken cancellation = default)
    {
        lock (VoiceScanGate)
        {
            // Share any pending OS query. A timed-out caller cannot cancel the native enumeration.
            if (voiceScan is null ||
                voiceScan.IsCompleted && (refresh || !voiceScan.IsCompletedSuccessfully))
                voiceScan = Task.Run(NativeSpeech.InstalledVoices);
            return voiceScan.WaitAsync(TimeSpan.FromSeconds(4), cancellation);
        }
    }

    internal static async Task<(VoiceChoice[] Voices, string? Error)> ReadVoicesAsync(ReaderSession session, string? language)
    {
        var voices = new List<VoiceChoice>();
        string? error = null;
        if (language is not null)
        {
            try
            {
                var installed = await ReadSystemVoicesAsync();
                voices.AddRange(SpeechVoice.ForLanguage(installed, language).Select(voice =>
                    new VoiceChoice(voice.Id, $"{voice.Name} ({voice.Language})")));
                if (voices.Count > 0)
                    voices.Insert(0, new VoiceChoice(SystemDefaultVoice, "Системний вибір"));
            }
            catch (TimeoutException) { error = "Система не повернула список голосів. Спробуйте знову пізніше."; }
            catch (Exception problem) { error = $"Не вдалося отримати системні голоси. {problem.Message}"; }

            try
            {
                var installed = await session.OptionalModelsAsync();
                string? InstalledRevision(string engine) => installed.FirstOrDefault(model => model.Id == engine && model.FilesPresent)?.Revision;
                if (language == "uk" && InstalledRevision("piper-uk") is { } piper)
                {
                    voices.Add(new("piper-uk:0", "Лада · Piper", "piper-uk", 0, piper));
                    voices.Add(new("piper-uk:1", "Микита · Piper", "piper-uk", 1, piper));
                    voices.Add(new("piper-uk:2", "Тетяна · Piper", "piper-uk", 2, piper));
                }
                if (DocumentLanguages.All.Any(item => item.Code == language) && InstalledRevision("supertonic-3") is { } supertonic)
                {
                    voices.Add(new("supertonic-3:0", "M1 · Supertonic", "supertonic-3", 0, supertonic));
                    voices.Add(new("supertonic-3:1", "F1 · Supertonic", "supertonic-3", 1, supertonic));
                }
            }
            catch (Exception problem) { error ??= $"Не вдалося отримати локальні голоси. {problem.Message}"; }
        }
        return (voices.ToArray(), error);
    }

    internal static VoiceChoice? AutomaticVoice(VoiceChoice[] choices, string? language) =>
        Array.Find(choices, voice => voice.Id == SystemDefaultVoice)
        ?? (language == "uk" ? Array.Find(choices, voice => voice.Engine == "piper-uk") : null)
        ?? Array.Find(choices, voice => voice.Engine == "supertonic-3");

    internal static async Task<string> PrepareSystemSpeechAsync(ReaderSession session, string language, string text)
    {
        if (language != "uk" || Encoding.UTF8.GetByteCount(text) > 64 * 1024) return text;
        try { return await session.PrepareUkrainianSpeechAsync(text); }
        catch (CoreException) { return text; }
    }

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

    internal Task<string?> PlayWaveAsync(SpeechAudio audio, int startAtMs, Action<int> started,
        Action? interrupted = null) => playback.PlayAsync(async token =>
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
        var wav = audio.WavBytes;
        if (firstSample > 0)
        {
            var payloadBytes = (audio.Samples - firstSample) * 2;
            wav = new byte[44 + payloadBytes];
            audio.WavBytes.AsSpan(0, 44).CopyTo(wav);
            audio.WavBytes.AsSpan(44 + firstSample * 2).CopyTo(wav.AsSpan(44));
            BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(4, 4), wav.Length - 8);
            BinaryPrimitives.WriteInt32LittleEndian(wav.AsSpan(40, 4), payloadBytes);
        }
        await wave.PlayAsync(wav, audio.PlaybackRate, () => started(startAtMs), token, interrupted);
        return CompletedStatus;
    });

    public bool Pause() => wave.Pause() || speech.Pause();
    public bool Resume() => wave.Resume() || speech.Resume();
    public void Stop() => playback.Stop();
    public void Dispose() => Stop();
}
