using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Practice;

public partial class PracticePage
{
    private readonly LocalSpeech pronunciation = new();
    private CancellationTokenSource? pronunciationRun;

    private void StopPronunciation()
    {
        var previous = pronunciationRun;
        pronunciationRun = null;
        previous?.Cancel();
        pronunciation.Stop();
        PronunciationStatus.IsVisible = false;
    }

    private async void SpeakPracticeSideClicked(object? sender, EventArgs args)
    {
        if (!visible || loading || grading || progress is not { Question: { } question } state ||
            sender is not Button { CommandParameter: string side } ||
            (side == "answer" && !question.Revealed))
            return;
        var session = AppState.Session;
        var card = session.Snapshot.Vocabulary.FirstOrDefault(entry => entry.Id == question.CardId);
        if (card is null) return;

        StopPronunciation();
        using var run = new CancellationTokenSource();
        pronunciationRun = run;
        var token = run.Token;
        var answer = side == "answer";
        var reverse = state.Direction == "reverse";
        var language = answer == reverse ? card.Language : card.TargetLanguage;
        var text = answer ? string.Join(". ", question.Answers) : question.Prompt;
        bool Current() => visible && ReferenceEquals(pronunciationRun, run) && ReferenceEquals(progress, state);

        PronunciationStatus.Text = $"Озвучуємо · {DocumentLanguages.Name(language)}…";
        PronunciationStatus.IsVisible = true;
        try
        {
            var preferences = await Task.Run(session.GetReaderPreferences, token);
            var (voices, error) = await LocalSpeech.ReadVoicesAsync(session, language).WaitAsync(token);
            if (!Current()) return;
            pronunciation.Rate = (preferences.SpeechRatePercent ?? LocalSpeech.DefaultRatePercent) / 100f;
            var preferred = preferences.SpeechVoices.GetValueOrDefault(language);
            var voice = voices.FirstOrDefault(item => item.Id == preferred) ?? LocalSpeech.AutomaticVoice(voices, language);
            if (voice is null)
                throw new InvalidOperationException(error ?? "Голос цієї мови відсутній. Локальні голоси можна завантажити в Налаштуваннях.");
            string? result = null;
            if (voice.Engine is { } engine)
            {
                try
                {
                    var audio = await session.SynthesizeSpeechTextAsync(
                        text, language, voice.Speaker, pronunciation.Rate, engine, token);
                    if (!Current()) return;
                    result = await pronunciation.PlayWaveAsync(audio, 0, _ => { });
                }
                catch (Exception failed) when (!token.IsCancellationRequested)
                {
                    result = failed.Message;
                }
            }
            if (!Current()) return;
            if (voice.Engine is null || (result is not null && result != LocalSpeech.CompletedStatus &&
                voices.Any(item => item.Id == LocalSpeech.SystemDefaultVoice)))
            {
                var spoken = await LocalSpeech.PrepareSystemSpeechAsync(session, language, text).WaitAsync(token);
                if (!Current()) return;
                pronunciation.PreferredVoice = voice.Engine is null && voice.Id != LocalSpeech.SystemDefaultVoice ? voice.Id : null;
                result = await pronunciation.SpeakAsync(spoken, language);
            }
            if (!Current()) return;
            PronunciationStatus.Text = result ?? "";
            PronunciationStatus.IsVisible = result is not null && result != LocalSpeech.CompletedStatus;
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception error)
        {
            if (!Current()) return;
            PronunciationStatus.Text = error.Message;
        }
        finally
        {
            if (ReferenceEquals(pronunciationRun, run)) pronunciationRun = null;
        }
    }
}
