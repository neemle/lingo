using AVFoundation;

namespace Reader.Native.Services;

internal sealed class NativeSpeech
{
    private AVSpeechSynthesizer? active;

    internal bool Pause() => active?.PauseSpeaking(AVSpeechBoundary.Immediate) == true;
    internal bool Resume() => active?.ContinueSpeaking() == true;

    internal async Task SpeakAsync(string text, string language, float rate, Action<int, int>? boundary,
        CancellationToken token)
    {
        var voice = AVSpeechSynthesisVoice.GetSpeechVoices().FirstOrDefault(item =>
            item.Language.Equals(language, StringComparison.OrdinalIgnoreCase) ||
            item.Language.StartsWith(language + "-", StringComparison.OrdinalIgnoreCase))
            ?? throw new InvalidOperationException("Голос цієї мови не встановлено в системі.");

        using var utterance = new AVSpeechUtterance(text)
        {
            Voice = voice,
            Rate = Math.Clamp(AVSpeechUtterance.DefaultSpeechRate * rate,
                AVSpeechUtterance.MinimumSpeechRate, AVSpeechUtterance.MaximumSpeechRate),
        };
        using var engine = new AVSpeechSynthesizer();
        var completed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        engine.DidFinishSpeechUtterance += (_, _) => completed.TrySetResult();
        engine.DidCancelSpeechUtterance += (_, _) => completed.TrySetCanceled();
        engine.WillSpeakRangeOfSpeechString += (_, args) =>
        {
            var span = args.CharacterRange;
            if (!token.IsCancellationRequested && span.Location >= 0 && span.Length > 0 &&
                span.Location <= text.Length && span.Length <= text.Length - span.Location)
                boundary?.Invoke((int)span.Location, (int)span.Length);
        };

        using var registration = token.Register(() => completed.TrySetCanceled(token));
        active = engine;
        try
        {
            token.ThrowIfCancellationRequested();
            engine.SpeakUtterance(utterance);
            await completed.Task;
        }
        finally
        {
            active = null;
            if (engine.Speaking || engine.Paused) engine.StopSpeaking(AVSpeechBoundary.Immediate);
        }
    }
}
