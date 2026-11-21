namespace Reader.Native.Services;

public sealed class LocalSpeech : IDisposable
{
    private CancellationTokenSource? cancellation;
    public float Rate { get; set; } = 1;
    public string Status { get; private set; } = "Використовуються голоси операційної системи.";

    public async Task SpeakAsync(string text, string language)
    {
        Stop();
        cancellation = new CancellationTokenSource();
        var token = cancellation.Token;
        try
        {
            var locales = await TextToSpeech.Default.GetLocalesAsync();
            var locale = locales.FirstOrDefault(item => item.Language.StartsWith(language,
                StringComparison.OrdinalIgnoreCase));
            if (locale is null) { Status = "Голос цієї мови не встановлено в системі."; return; }
            Status = "Озвучення поточного речення. Точні межі слів недоступні.";
            await TextToSpeech.Default.SpeakAsync(text, new SpeechOptions { Locale = locale, Rate = Rate }, token);
            Status = "Озвучення завершено.";
        }
        catch (OperationCanceledException) { Status = "Озвучення зупинено."; }
        catch (Exception error) { Status = $"Озвучення недоступне: {error.Message}"; }
    }

    public void Stop()
    {
        cancellation?.Cancel();
        cancellation?.Dispose();
        cancellation = null;
    }

    public void Dispose() => Stop();
}
