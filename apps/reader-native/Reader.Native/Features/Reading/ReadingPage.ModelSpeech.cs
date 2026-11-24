using Microsoft.Maui.Dispatching;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record SpeechSource(string Document, string Sentence, string Language, string SourceText, string SpeechText);
    private sealed record ClipKey(SpeechSource Source, string Engine, uint Speaker, float Rate);

    private VoiceChoice[] availableVoices = [];
    private string? voiceLanguage;
    private Task loadingVoices = Task.CompletedTask;
    private int voiceCatalogRevision;
    private readonly SemaphoreSlim modelInference = new(1);
    private CancellationTokenSource? modelSpeechRun;
    private SpeechSource? activeSpeechSource;
    private Action? checkpointModelSpeech;

    private void RefreshVoices(bool force = false)
    {
        var language = AppState.Session.Document?.Language;
        if (!force && voiceLanguage == language) return;
        voiceLanguage = language;
        VoicePicker.IsEnabled = DetachedVoicePicker.IsEnabled = false;
        loadingVoices = LoadVoicesAsync(language, ++voiceCatalogRevision);
    }

    private void VoiceResourcesChanged()
    {
        if (readingPageActive) RefreshVoices(force: true);
    }

    private async Task WaitForSpeechVoicesAsync(CancellationToken token = default)
    {
        while (true)
        {
            var pending = loadingVoices;
            await pending.WaitAsync(token);
            if (ReferenceEquals(pending, loadingVoices)) return;
        }
    }

    private async Task LoadVoicesAsync(string? language, int revision)
    {
        var (voices, error) = await LocalSpeech.ReadVoicesAsync(AppState.Session, language);
        if (revision != voiceCatalogRevision) return;

        if (SelectedVoice() is { Engine: not null } previous &&
            !voices.Any(voice => voice.Id == previous.Id && voice.Revision == previous.Revision))
        {
            if (utteranceActive) StopSpeech();
            if (speakingDetachedSentence) ClearDetachedSpeech();
            StopWordDetail();
        }

        loadingSpeechPreferences = true;
        try
        {
            availableVoices = voices;
            const string automaticLabel = "Автоматично";
            VoicePicker.ItemsSource = DetachedVoicePicker.ItemsSource =
                new[] { automaticLabel }.Concat(voices.Select(voice => voice.Label)).ToArray();
            var saved = language is not null && readerPreferences.SpeechVoices.TryGetValue(language, out var stored)
                ? stored : null;
            var chosen = Array.FindIndex(availableVoices, voice => voice.Id == saved);
            VoicePicker.SelectedIndex = DetachedVoicePicker.SelectedIndex = chosen + 1;
            VoicePicker.IsEnabled = DetachedVoicePicker.IsEnabled = voices.Length > 0;
            SetVoiceStatus(error ?? (language is null ? ""
                : voices.Length == 0 ? "Голос цієї мови відсутній. Локальні голоси можна завантажити в Налаштуваннях."
                : chosen < 0 && !string.IsNullOrEmpty(saved) ? "Збережений голос недоступний. Використано автоматичний вибір."
                : ModelVoiceStatus()));
        }
        catch (Exception problem)
        {
            availableVoices = [];
            VoicePicker.IsEnabled = DetachedVoicePicker.IsEnabled = false;
            SetVoiceStatus($"Не вдалося налаштувати голоси. {problem.Message}");
        }
        finally { loadingSpeechPreferences = false; }
        if (!utteranceActive) RefreshSpeechResume();
        if (detachedAddress is not null) UpdateDetachedControls();
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
        if (speakingDetachedSentence) ClearDetachedSpeech();
        StopWordDetail();
        if (DetachedVoicePicker.SelectedIndex != VoicePicker.SelectedIndex)
            DetachedVoicePicker.SelectedIndex = VoicePicker.SelectedIndex;
        SaveReaderPreference(ReaderPreferenceChange.SpeechVoice(voiceLanguage, PickedVoice()?.Id));
        SetVoiceStatus(ModelVoiceStatus());
        RefreshSpeechResume();
        if (detachedAddress is not null) UpdateDetachedControls();
    }

    private void DetachedVoiceChanged(object? sender, EventArgs args)
    {
        if (!loadingSpeechPreferences && voiceLanguage is not null && DetachedVoicePicker.SelectedIndex >= 0
            && VoicePicker.SelectedIndex != DetachedVoicePicker.SelectedIndex)
            VoicePicker.SelectedIndex = DetachedVoicePicker.SelectedIndex;
    }

    private string ModelVoiceStatus()
    {
        var voice = SelectedVoice();
        if (voice?.Engine is not null) return $"{voice.Label} · підсвічування речення";
        return voiceLanguage == "uk" ? "Системний голос може ігнорувати наголоси." : "";
    }

    private VoiceChoice? PickedVoice() => VoicePicker.SelectedIndex > 0 && VoicePicker.SelectedIndex <= availableVoices.Length
        ? availableVoices[VoicePicker.SelectedIndex - 1] : null;

    private VoiceChoice? SelectedVoice() => PickedVoice() ?? LocalSpeech.AutomaticVoice(availableVoices, voiceLanguage);

    private async Task<VoiceChoice?> WordDetailVoiceAsync(string language, CancellationToken token)
    {
        if (language == voiceLanguage)
        {
            await WaitForSpeechVoicesAsync(token);
            if (language == voiceLanguage) return SelectedVoice();
        }
        var (voices, _) = await LocalSpeech.ReadVoicesAsync(AppState.Session, language).WaitAsync(token);
        var saved = readerPreferences.SpeechVoices.GetValueOrDefault(language);
        return voices.FirstOrDefault(voice => voice.Id == saved) ?? LocalSpeech.AutomaticVoice(voices, language);
    }

    private static string ModelTimelineRevision(VoiceChoice voice, string language)
    {
        var profile = voice.Engine switch
        {
            "supertonic-3" when language == "uk" => 10,
            "supertonic-3" => 9,
            _ => 5,
        };
        return $"{voice.Revision}:narration-v{profile}";
    }

    private static int? ModelResumePosition(SpeechResume? resume, VoiceChoice? voice, string language, int ratePercent) =>
        resume?.Model is { } model && voice?.Revision is not null &&
        model.Revision == ModelTimelineRevision(voice, language) &&
        model.VoiceId == voice.Id && model.Language == language &&
        SpeechProgress.SameModelTimelineRate(model.RatePercent, ratePercent) &&
        model.PositionMs is > 0 and < LocalSpeech.MaxModelAudioSeconds * 1000
            ? model.PositionMs : null;

    private static SpeechSource SpeechSourceFor(Document document, Sentence sentence) =>
        new(document.Id, sentence.Id, document.Language, sentence.Text, EffectiveSpeechText(document, sentence));

    private bool IsSpeechSourceCurrent(ReaderSession session, SpeechSource source) =>
        session.Document is { } document && session.Sentence is { } sentence &&
        source == SpeechSourceFor(document, sentence);

    private async Task<string?> PlayModelAsync(ReaderSession session, Document document, Sentence sentence,
        VoiceChoice voice, int ratePercent, int startAtMs, string textHash, int request, bool singleSentence,
        CancellationToken token)
    {
        var source = SpeechSourceFor(document, sentence);
        var key = new ClipKey(source, voice.Engine!, voice.Speaker, ratePercent / 100f);
        var audio = PrepareAudio(session, key, token);
        IDispatcherTimer? timer = null;
        var playbackStartMs = 0;
        var lastCheckpointMs = startAtMs;
        void CapturePosition()
        {
            if (request != speechRequest || !IsSpeechSourceCurrent(session, source) || speech.WavePosition is not { } position)
                return;
            var millis = playbackStartMs + (int)position.TotalMilliseconds;
            if (millis <= lastCheckpointMs || millis >= LocalSpeech.MaxModelAudioSeconds * 1000) return;
            lastCheckpointMs = millis;
            var model = new ModelSpeechResume(voice.Id, ModelTimelineRevision(voice, document.Language), document.Language, ratePercent, millis);
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
                if (!singleSentence && ContinueSpeech.IsChecked)
                    _ = PrefetchNextAudioAsync(session, key, token);
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

    private Task<SpeechAudio> PrepareAudio(ReaderSession session, ClipKey key, CancellationToken token, int? page = null)
    {
        var task = GenerateAudioAsync(session, key, token, page);
        // The view can stop awaiting before the native job reaches a cancellation checkpoint.
        _ = task.ContinueWith(failed => { _ = failed.Exception; }, CancellationToken.None,
            TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
        return task;
    }

    private static async Task PrefetchNextAudioAsync(ReaderSession session, ClipKey current, CancellationToken token, int? page = null)
    {
        try
        {
            await session.PrefetchSpeechAsync(current.Source.Document, current.Source.Sentence,
                current.Speaker, current.Rate, current.Engine, token, page);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) {}
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error.Message);
        }
    }

    private async Task<SpeechAudio> GenerateAudioAsync(ReaderSession session, ClipKey key, CancellationToken token, int? page)
    {
        await modelInference.WaitAsync(token);
        try
        {
            token.ThrowIfCancellationRequested();
            return await session.SynthesizeSpeechAsync(key.Source.Document, key.Source.Sentence,
                key.Speaker, key.Rate, key.Engine, page, token);
        }
        finally { modelInference.Release(); }
    }

    private void CancelModelSpeech()
    {
        var previous = modelSpeechRun;
        modelSpeechRun = null;
        previous?.Cancel();
    }
}
