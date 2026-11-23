using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Practice;

public partial class PracticePage
{
    private readonly LocalSpeech pronunciation = new();
    private int pronunciationRequest;

    private void StopPronunciation()
    {
        pronunciationRequest++;
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
        var request = pronunciationRequest;
        var answer = side == "answer";
        var reverse = state.Direction == "reverse";
        var language = answer == reverse ? card.Language : card.TargetLanguage;
        var text = answer ? string.Join(". ", question.Answers) : question.Prompt;
        bool Current() => visible && pronunciationRequest == request && ReferenceEquals(progress, state);

        PronunciationStatus.Text = $"Озвучуємо · {DocumentLanguages.Name(language)}…";
        PronunciationStatus.IsVisible = true;
        try
        {
            var preferences = await Task.Run(session.GetReaderPreferences);
            var spoken = await LocalSpeech.PrepareSystemSpeechAsync(session, language, text);
            if (!Current()) return;
            pronunciation.Rate = (preferences.SpeechRatePercent ?? LocalSpeech.DefaultRatePercent) / 100f;
            var preferred = preferences.SpeechVoices.GetValueOrDefault(language);
            pronunciation.PreferredVoice = preferred is not null &&
                LocalSpeech.VoicesForLanguage(language).Any(voice => voice.Id == preferred) ? preferred : null;
            var result = await pronunciation.SpeakAsync(spoken, language);
            if (!Current()) return;
            PronunciationStatus.Text = result ?? "";
            PronunciationStatus.IsVisible = result is not null && result != LocalSpeech.CompletedStatus;
        }
        catch (Exception error)
        {
            if (!Current()) return;
            PronunciationStatus.Text = error.Message;
        }
    }
}
