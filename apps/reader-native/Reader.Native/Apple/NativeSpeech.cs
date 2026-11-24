using AVFoundation;
using Reader.Presentation;

namespace Reader.Native.Services;

internal sealed class NativeSpeech
{
    private static readonly SemaphoreSlim VoiceLookupGate = new(1);
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
        using var voice = await ResolveVoiceAsync(language, preferredVoice, token);
        token.ThrowIfCancellationRequested();

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

    private static async Task<AVSpeechSynthesisVoice> ResolveVoiceAsync(
        string language, string? preferredVoice, CancellationToken token)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(4));
        var request = deadline.Token;
        var result = new TaskCompletionSource<AVSpeechSynthesisVoice>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var registration = request.Register(() => result.TrySetCanceled(request));

        _ = Task.Run(async () =>
        {
            try
            {
                request.ThrowIfCancellationRequested();
                var voices = await LocalSpeech.ReadSystemVoicesAsync(refresh: false, cancellation: request);
                await VoiceLookupGate.WaitAsync(request);
                try
                {
                    var selected = voices.FirstOrDefault(voice => voice.Id == preferredVoice &&
                        SpeechVoice.MatchesLanguage(voice.Language, language))?.Id;
                    if (selected is null)
                    {
                        using var systemVoice = AVSpeechSynthesisVoice.FromLanguage(language);
                        selected = SpeechVoice.ResolveId(voices, language, null, systemVoice?.Identifier);
                    }
                    request.ThrowIfCancellationRequested();
                    var voice = selected is null ? null : AVSpeechSynthesisVoice.FromIdentifier(selected);
                    if (voice is null)
                        throw new InvalidOperationException("Голос цієї мови не встановлено в системі.");
                    if (!result.TrySetResult(voice)) voice.Dispose();
                }
                finally { VoiceLookupGate.Release(); }
            }
            catch (Exception error) { result.TrySetException(error); }
        });
        try { return await result.Task; }
        catch (Exception error) when (!token.IsCancellationRequested && error is OperationCanceledException or TimeoutException)
        {
            throw new TimeoutException("Система не підготувала голос вчасно. Спробуйте знову пізніше.", error);
        }
    }
}
