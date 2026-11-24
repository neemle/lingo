using AVFoundation;
using Reader.Presentation;

namespace Reader.Native.Services;

internal sealed class NativeSpeech
{
    private AVSpeechSynthesizer? active;

    internal bool Pause() => active?.PauseSpeaking(AVSpeechBoundary.Immediate) == true;
    internal bool Resume() => active?.ContinueSpeaking() == true;

    internal static SpeechVoice[] InstalledVoices() => AVSpeechSynthesisVoice.GetSpeechVoices()
        .Select(ToVoice).ToArray();

    private static SpeechVoice ToVoice(AVSpeechSynthesisVoice voice) =>
        new(voice.Identifier, voice.Name, voice.Language);

    internal async Task SpeakAsync(string text, string language, string? preferredVoice, float rate,
        Action<int, int>? boundary, CancellationToken token)
    {
        var voices = AVSpeechSynthesisVoice.GetSpeechVoices();
        using var systemVoice = AVSpeechSynthesisVoice.FromLanguage(language);
        var selected = SpeechVoice.ResolveId(voices.Select(ToVoice), language, preferredVoice, systemVoice?.Identifier);
        var voice = voices.FirstOrDefault(item => item.Identifier == selected)
            ?? throw new InvalidOperationException("Голос цієї мови не встановлено в системі.");

        using var utterance = new AVSpeechUtterance(text)
        {
            Voice = voice,
            PostUtteranceDelay = 0.18,
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
