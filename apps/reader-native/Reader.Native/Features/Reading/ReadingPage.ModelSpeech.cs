using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record VoiceChoice(string Id, string Label, string? Engine = null, uint Speaker = 0);
    private sealed record SpeechSource(string Document, string Sentence, string Language, string Text);
    private sealed record ClipKey(SpeechSource Source, string Engine, uint Speaker, float Rate);
    private sealed record PendingClip(ClipKey Key, Task<SpeechAudio> Audio);

    private VoiceChoice[] availableVoices = [];
    private string? voiceLanguage;
    private Task loadingVoices = Task.CompletedTask;
    private int voiceCatalogRevision;
    private readonly SemaphoreSlim modelInference = new(1);
    private CancellationTokenSource? modelSpeechRun;
    private PendingClip? nextClip;
    private SpeechSource? activeSpeechSource;

    private static string VoicePreference(string language) => $"reader.speech.voice.{language}";

    private void RefreshVoices(bool force = false)
    {
        var language = AppState.Session.Document?.Language;
        if (!force && voiceLanguage == language) return;
        voiceLanguage = language;
        VoicePicker.IsEnabled = false;
        loadingVoices = LoadVoicesAsync(language, ++voiceCatalogRevision);
    }

    private async Task LoadVoicesAsync(string? language, int revision)
    {
        var voices = new List<VoiceChoice>();
        string? error = null;
        if (language is not null)
        {
            try
            {
                voices.AddRange(LocalSpeech.VoicesForLanguage(language).Select(voice =>
                    new VoiceChoice(voice.Id, $"{voice.Name} ({voice.Language})")));
            }
            catch (Exception problem) { error = $"Не вдалося отримати системні голоси. {problem.Message}"; }

            try
            {
                var installed = await AppState.Session.OptionalModelsAsync();
                bool Installed(string engine) => installed.Any(model => model.Id == engine && model.FilesPresent);
                if (language == "uk" && Installed("piper-uk"))
                {
                    voices.Add(new("piper-uk:0", "Лада · Piper", "piper-uk", 0));
                    voices.Add(new("piper-uk:1", "Микита · Piper", "piper-uk", 1));
                    voices.Add(new("piper-uk:2", "Тетяна · Piper", "piper-uk", 2));
                }
                if (DocumentLanguages.All.Any(item => item.Code == language) && Installed("supertonic-3"))
                {
                    voices.Add(new("supertonic-3:0", "M1 · Supertonic", "supertonic-3", 0));
                    voices.Add(new("supertonic-3:1", "F1 · Supertonic", "supertonic-3", 1));
                }
            }
            catch (Exception problem) { error ??= $"Не вдалося отримати локальні голоси. {problem.Message}"; }
        }
        if (revision != voiceCatalogRevision) return;

        loadingSpeechPreferences = true;
        try
        {
            availableVoices = voices.ToArray();
            VoicePicker.ItemsSource = new[] { "Системний вибір" }.Concat(voices.Select(voice => voice.Label)).ToArray();
            var saved = language is null ? "" : Preferences.Get(VoicePreference(language), "");
            var chosen = Array.FindIndex(availableVoices, voice => voice.Id == saved);
            VoicePicker.SelectedIndex = chosen + 1;
            VoicePicker.IsEnabled = voices.Count > 0;
            SetVoiceStatus(error ?? (language is null ? ""
                : voices.Count == 0 ? "Голос цієї мови відсутній. Локальні голоси можна завантажити в Налаштуваннях."
                : chosen < 0 && saved.Length > 0 ? "Збережений голос недоступний. Використано системний вибір."
                : ModelVoiceStatus()));
        }
        catch (Exception problem)
        {
            availableVoices = [];
            VoicePicker.IsEnabled = false;
            SetVoiceStatus($"Не вдалося налаштувати голоси. {problem.Message}");
        }
        finally { loadingSpeechPreferences = false; }
        if (!utteranceActive) RefreshSpeechResume();
    }

    private void SetVoiceStatus(string status)
    {
        VoiceStatus.Text = status;
        VoiceStatus.IsVisible = status.Length > 0;
    }

    private void VoiceChanged(object? sender, EventArgs args)
    {
        if (loadingSpeechPreferences || voiceLanguage is null || VoicePicker.SelectedIndex < 0) return;
        if (utteranceActive) StopSpeech();
        Preferences.Set(VoicePreference(voiceLanguage), SelectedVoice()?.Id ?? "");
        SetVoiceStatus(ModelVoiceStatus());
        RefreshSpeechResume();
    }

    private string ModelVoiceStatus() => SelectedVoice()?.Engine is null ? "" : "Локальний голос · підсвічування речення";

    private VoiceChoice? SelectedVoice() => VoicePicker.SelectedIndex > 0 && VoicePicker.SelectedIndex <= availableVoices.Length
        ? availableVoices[VoicePicker.SelectedIndex - 1] : null;

    private static SpeechSource SpeechSourceFor(Document document, Sentence sentence) =>
        new(document.Id, sentence.Id, document.Language, sentence.Text);

    private bool IsSpeechSourceCurrent(ReaderSession session, SpeechSource source) =>
        session.Document is { } document && session.Sentence is { } sentence &&
        source == SpeechSourceFor(document, sentence);

    private async Task<string?> PlayModelAsync(ReaderSession session, Document document, Sentence sentence,
        VoiceChoice voice, float rate, int request, CancellationToken token)
    {
        var source = SpeechSourceFor(document, sentence);
        var key = new ClipKey(source, voice.Engine!, voice.Speaker, rate);
        var previous = nextClip;
        nextClip = null;
        var audio = previous?.Key == key ? previous.Audio : PrepareAudio(session, key, token);
        try
        {
            var clip = await audio.WaitAsync(token);
            if (request != speechRequest || !IsSpeechSourceCurrent(session, source)) return null;
            return await speech.PlayWaveAsync(clip, () =>
            {
                if (request != speechRequest || token.IsCancellationRequested) return;
                PauseSpeech.IsEnabled = true;
                SpeechStatus.Text = "Озвучення поточного речення…";
                if (ContinueSpeech.IsChecked && document.Position + 1 < document.Sentences.Length)
                {
                    var next = key with { Source = SpeechSourceFor(document, document.Sentences[document.Position + 1]) };
                    nextClip = new(next, PrepareAudio(session, next, token));
                }
            });
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { return null; }
        catch (Exception error) { return $"Локальний голос недоступний. {error.Message}"; }
    }

    private Task<SpeechAudio> PrepareAudio(ReaderSession session, ClipKey key, CancellationToken token)
    {
        var task = GenerateAudioAsync(session, key, token);
        // A stopped or superseded request may leave a native job running to completion.
        _ = task.ContinueWith(failed => { _ = failed.Exception; }, CancellationToken.None,
            TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
        return task;
    }

    private async Task<SpeechAudio> GenerateAudioAsync(ReaderSession session, ClipKey key, CancellationToken token)
    {
        await modelInference.WaitAsync(token);
        try
        {
            token.ThrowIfCancellationRequested();
            return await session.SynthesizeSpeechAsync(key.Source.Document, key.Source.Sentence, key.Speaker, key.Rate, key.Engine);
        }
        finally { modelInference.Release(); }
    }

    private void CancelModelSpeech()
    {
        var previous = modelSpeechRun;
        modelSpeechRun = null;
        nextClip = null;
        previous?.Cancel();
    }
}
