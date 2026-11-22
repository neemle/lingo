using Microsoft.Maui.Dispatching;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record VoiceChoice(string Id, string Label, string? Engine = null, uint Speaker = 0, string? Revision = null);
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
    private Action? checkpointModelSpeech;

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

    private static int? ModelResumePosition(SpeechResume? resume, VoiceChoice? voice, string language, int ratePercent) =>
        resume?.Model is { } model && voice?.Revision is { } revision && model.Revision == revision &&
        model.VoiceId == voice.Id && model.Language == language && model.RatePercent == ratePercent &&
        model.PositionMs is > 0 and < 45_000
            ? model.PositionMs : null;

    private static SpeechSource SpeechSourceFor(Document document, Sentence sentence) =>
        new(document.Id, sentence.Id, document.Language, sentence.Text);

    private bool IsSpeechSourceCurrent(ReaderSession session, SpeechSource source) =>
        session.Document is { } document && session.Sentence is { } sentence &&
        source == SpeechSourceFor(document, sentence);

    private async Task<string?> PlayModelAsync(ReaderSession session, Document document, Sentence sentence,
        VoiceChoice voice, int ratePercent, int startAtMs, string textHash, int request, CancellationToken token)
    {
        var source = SpeechSourceFor(document, sentence);
        var key = new ClipKey(source, voice.Engine!, voice.Speaker, ratePercent / 100f);
        var previous = nextClip;
        nextClip = null;
        var audio = previous?.Key == key ? previous.Audio : PrepareAudio(session, key, token);
        IDispatcherTimer? timer = null;
        var playbackStartMs = 0;
        var lastCheckpointMs = startAtMs;
        void CapturePosition()
        {
            if (request != speechRequest || !IsSpeechSourceCurrent(session, source) || speech.WavePosition is not { } position)
                return;
            var millis = playbackStartMs + (int)position.TotalMilliseconds;
            if (millis <= lastCheckpointMs || millis >= 45_000) return;
            lastCheckpointMs = millis;
            var model = new ModelSpeechResume(voice.Id, voice.Revision!, document.Language, ratePercent, millis);
            pendingSpeechResume = (document.Id, new(sentence.Id, textHash, 0, model));
        }
        Action capture = CapturePosition;
        try
        {
            var clip = await audio.WaitAsync(token);
            if (request != speechRequest || !IsSpeechSourceCurrent(session, source)) return null;
            return await speech.PlayWaveAsync(clip, startAtMs, actualStartMs =>
            {
                if (request != speechRequest || token.IsCancellationRequested) return;
                playbackStartMs = lastCheckpointMs = actualStartMs;
                checkpointModelSpeech = capture;
                timer = Dispatcher.CreateTimer();
                timer.Interval = TimeSpan.FromSeconds(2);
                timer.Tick += (_, _) => CheckpointSpeechPosition();
                timer.Start();
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
        finally
        {
            timer?.Stop();
            if (checkpointModelSpeech == capture) checkpointModelSpeech = null;
        }
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
