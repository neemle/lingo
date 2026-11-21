using Reader.Presentation;

namespace Reader.Native.Services;

public sealed class LocalSpeech : IDisposable
{
    private readonly SpeechPlayback playback = new();
    public float Rate { get; set; } = 1;

    public Task<string?> SpeakAsync(string text, string language)
    {
        var rate = Rate;
        return playback.PlayAsync(token => SpeakCoreAsync(text, language, rate, token));
    }

    private static async Task<string> SpeakCoreAsync(string text, string language, float rate, CancellationToken token)
    {
        var locales = await TextToSpeech.Default.GetLocalesAsync();
        token.ThrowIfCancellationRequested();
        var locale = locales.FirstOrDefault(item => item.Language.StartsWith(language,
            StringComparison.OrdinalIgnoreCase));
        if (locale is null) return "Голос цієї мови не встановлено в системі.";
        await TextToSpeech.Default.SpeakAsync(text, new SpeechOptions { Locale = locale, Rate = rate }, token);
        token.ThrowIfCancellationRequested();
        return "Озвучення завершено. Точні межі слів недоступні.";
    }

    public void Stop() => playback.Stop();
    public void Dispose() => Stop();
}
