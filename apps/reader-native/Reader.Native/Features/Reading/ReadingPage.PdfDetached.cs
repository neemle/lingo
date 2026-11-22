using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record DetachedAddress(string Document, int Page, string Text, string Language);
    private readonly LocalSpeech detachedSpeech = new();
    private DetachedAddress? detachedAddress;
    private DetachedPdfSentence? detachedSentence;
    private TextSelection? detachedSelection;
    private bool loadingDetachedSentence;
    private int detachedSentenceRequest;
    private bool changingDetachedAnnotation;
    private bool savingDetachedWord;
    private bool translatingDetachedSentence;
    private DetachedPdfTranslation? detachedMachineTranslation;
    private int? detachedCacheLookupRun;
    private bool speakingDetachedSentence;
    private bool detachedPaused;
    private int detachedAudioRun;
    private Action? checkpointDetachedSpeech;
    private CancellationTokenSource? detachedAudioCancellation;
    private CancellationTokenSource? detachedSpeechTravel;
    private int detachedTranslationRun;

#if MACCATALYST
    private void ShowDetachedPdfPage(Document document, PdfReadPage page)
    {
        TargetLanguage.IsEnabled = !AppState.Session.Busy;
        var address = new DetachedAddress(document.Id, page.Page, page.Text, document.Language);
        if (detachedAddress == address) return;

        StopDetachedPdfReader();
        detachedAddress = address;
        detachedInitialSentence = LoadDetachedSentenceAsync(address, position: null, automatic: true);
    }
#endif

    private void StopDetachedPdfReader()
    {
        if (detachedAddress is null) return;
        detachedAddress = null;
        detachedSentence = null;
        StopAnalysisPrefetch();
        SetDetachedSelection(null);
        UpdateDetachedExplainButton();
        loadingDetachedSentence = false;
        ClearDetachedSpeech();
        ResetDetachedTranslation();
    }

    private async Task LoadDetachedSentenceAsync(
        DetachedAddress address, int? position, string? sentenceId = null, bool automatic = false)
    {
        if (!automatic) CancelDetachedSpeechTravel();
        var request = ++detachedSentenceRequest;
        loadingDetachedSentence = true;
        SyncAnalysisPrefetch();
        SetDetachedSelection(null);
        UpdateDetachedExplainButton();
        if (!automatic) ClearDetachedSpeech();
        ResetDetachedTranslation();
        try
        {
            var result = await AppState.Session.DetachedPdfSentenceAsync(address.Document, address.Page, position, sentenceId);
            if (request != detachedSentenceRequest || !ReferenceEquals(detachedAddress, address)) return;
            detachedSentence = result;
            if (result is not { } current) return;
            var sentence = current.Sentence;
            DetachedPdfText.HighlightStart = sentence.StartUtf16;
            DetachedPdfText.HighlightLength = sentence.EndUtf16 - sentence.StartUtf16;
            DetachedPdfText.RevealOffset(sentence.StartUtf16);
            DetachedSentenceStatus.Text = $"Речення {current.Position + 1} із {current.Total}";
        }
        catch (Exception error)
        {
            if (request == detachedSentenceRequest && ReferenceEquals(detachedAddress, address)) DetachedSentenceStatus.Text = error.Message;
        }
        finally
        {
            if (request == detachedSentenceRequest && ReferenceEquals(detachedAddress, address))
            {
                loadingDetachedSentence = false;
                SyncAnalysisPrefetch();
                ShowDetachedReviewedTranslation();
                UpdateDetachedControls();
                _ = RestoreDetachedTranslationAsync();
            }
        }
    }

    private async void PreviousDetachedSentenceClicked(object? sender, EventArgs args) => await MoveDetachedSentenceAsync(-1);
    private async void NextDetachedSentenceClicked(object? sender, EventArgs args) => await MoveDetachedSentenceAsync(1);

    private Task MoveDetachedSentenceAsync(int change)
    {
        if (loadingDetachedSentence || detachedAddress is not { } address || detachedSentence is not { } current)
            return Task.CompletedTask;
        var next = current.Position + change;
        return next >= 0 && next < current.Total ? LoadDetachedSentenceAsync(address, next) : Task.CompletedTask;
    }

    private void DetachedPdfSelected(object? sender, TextSelection selected)
    {
        if (detachedAddress is null || loadingDetachedSentence) return;
        SetDetachedSelection(selected);
        UpdateDetachedControls();
    }

    private void SetDetachedSelection(TextSelection? selected)
    {
        if (detachedSelection != selected) detachedWordTranslation = null;
        detachedSelection = selected;
        DetachedWordExplanationText.IsVisible = false;
    }

    private async void DetachedAnalyzeClicked(object? sender, EventArgs args)
    {
        if (!DetachedAnalyze.IsEnabled || detachedAddress is not { } address || detachedSentence is not { } current) return;
        var session = AppState.Session;
        await ShowAnalysisPageAsync("Перевірка речення", current.Sentence.Text,
            start => session.AnalyzeDetachedPdfSentenceAsync(address.Document, address.Page, current.Sentence.Id, start),
            (model, cachedOnly) => session.ReviewSentenceAsync(model, address.Document, address.Page, current.Sentence.Id,
                revisionId: current.RevisionId, cachedOnly: cachedOnly));
    }

    private async void DetachedAnalyzeSelectionClicked(object? sender, EventArgs args)
    {
        if (!DetachedAnalyzeSelection.IsEnabled || detachedAddress is not { } address ||
            detachedSelection is not { } selected || detachedSentence is not { } current) return;
        var session = AppState.Session;
        await ShowAnalysisPageAsync("Перевірка виділення", selected.Text,
            _ => session.AnalyzeDetachedPdfSelectionAsync(address.Document, address.Page, selected),
            (model, cachedOnly) => session.ReviewSentenceAsync(model, address.Document, address.Page, null,
                new { kind = "selection", startUtf16 = selected.StartUtf16, text = selected.Text },
                revisionId: current.RevisionId, cachedOnly: cachedOnly));
    }

    private async void DetachedSaveWordClicked(object? sender, EventArgs args)
    {
        if (!DetachedSaveWord.IsEnabled || detachedAddress is not { } address ||
            detachedSelection is not { } selected || TargetCode is not { } target) return;

        await SaveDetachedWordAsync(address, selected, target);
    }

    private async Task SaveDetachedWordAsync(DetachedAddress address, TextSelection selected, string target)
    {
        if (savingDetachedWord || detachedSentence?.RevisionId is not { } revision) return;
        var session = AppState.Session;
        savingDetachedWord = true;
        UpdateDetachedControls();
        (string Target, string Text, string Source)? draft =
            detachedWordTranslation is { } candidate && candidate.Target == target ? candidate : null;
        var reconstructed = draft is { } result && result.Source != selected.Text.Trim() ? result.Source : null;
        try
        {
            WordLookup? lookup = null;
            if (reconstructed is null)
            {
                try { lookup = await session.LookupDetachedPdfWordAsync(address.Document, address.Page, selected); }
                catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
            }
            if (detachedAddress != address || detachedSelection != selected || TargetCode != target) return;

            SenseSnapshot? sense = null;
            if (lookup?.Meanings is { Length: > 0 } meanings)
            {
                var choice = await ChooseDetachedMeaningAsync(selected.Text, meanings);
                if (choice is null || !SelectionDocumentCurrent()) return;
                sense = choice.Sense;
            }

            var description = sense is null ? $"«{reconstructed ?? selected.Text}» · {DocumentLanguages.Name(target)}"
                : $"«{selected.Text}» · {sense.Gloss}{Environment.NewLine}Переклад · {DocumentLanguages.Name(target)}";
            var translation = await DisplayPromptAsync("Додати до словника", description,
                accept: "Далі", cancel: "Скасувати", placeholder: "Переклад",
                initialValue: sense is { } chosenSense && chosenSense.GlossLanguage == target ? chosenSense.Gloss
                    : draft?.Text,
                maxLength: 2000);
            if (string.IsNullOrWhiteSpace(translation) || !SelectionDocumentCurrent()) return;

            var reading = new VocabularyReadingOption("Як у тексті");
            if (sense is null && reconstructed is null)
            {
                LinguisticAnalysis? details = null;
                try { details = await session.AnalyzeDetachedPdfSelectionAsync(address.Document, address.Page, selected); }
                catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
                if (!SelectionDocumentCurrent()) return;

                var options = DetachedVocabularyReadings(details, selected, reading);
                if (options.Length > 1)
                {
                    var labels = options.Select((option, index) => $"{index + 1}. {option.Label}").ToArray();
                    var choice = await DisplayActionSheetAsync("Як зберегти картку", "Скасувати", null, labels);
                    var index = Array.IndexOf(labels, choice);
                    if (index < 0 || !SelectionDocumentCurrent()) return;
                    reading = options[index];
                }
            }

            await session.WaitUntilIdleAsync();
            if (!SelectionDocumentCurrent()) return;
            await session.SaveDetachedPdfWordAsync(address.Document, address.Page, selected, translation,
                target, reading.Lemma, reading.Expression, sense, revision, reconstructed);
            if (session.Error.Length > 0)
            {
                await DisplayAlertAsync("Словник", session.Error, "Гаразд");
                return;
            }
            if (detachedAddress is { } visible && visible.Document == address.Document && visible.Page == address.Page)
                DetachedSentenceStatus.Text = "Картку збережено в словнику.";
        }
        catch (Exception error) { await DisplayAlertAsync("Словник", error.Message, "Гаразд"); }
        finally
        {
            savingDetachedWord = false;
            UpdateDetachedControls();
        }

        bool SelectionDocumentCurrent() =>
            readingPageActive && session.Document is { } document && document.Id == address.Document &&
            document.Language == address.Language && TargetCode == target && ReferenceEquals(detachedAddress, address) &&
            detachedSelection == selected && detachedSentence?.RevisionId == revision;
    }

    private static VocabularyReadingOption[] DetachedVocabularyReadings(
        LinguisticAnalysis? details, TextSelection selected, VocabularyReadingOption surface)
    {
        if (details is not { Package: { } package }) return [surface];
        var version = $"{package.Version} · {package.Revision}";
        if (details.Words is [var word])
        {
            return [surface, .. word.Morphology.DistinctBy(analysis => (analysis.Lemma, analysis.PartOfSpeech))
                .Select(analysis => new VocabularyReadingOption($"Лема · {analysis.Lemma} · {analysis.PartOfSpeech}",
                    Lemma: new LemmaSnapshot(analysis.Lemma, selected.Text, analysis.PartOfSpeech, analysis.Features,
                        package.Name, version)))];
        }
        return [surface, .. details.Expressions.DistinctBy(expression => (expression.Canonical, expression.Kind))
            .Select(expression => new VocabularyReadingOption($"Сталий вираз · {expression.Text}",
                Expression: new ExpressionSnapshot(expression.Canonical, expression.Text, expression.Kind,
                    expression.Observations, package.Name, version)))];
    }

    private async void DetachedBookmarkClicked(object? sender, EventArgs args)
    {
        if (!DetachedBookmark.IsEnabled || detachedAddress is not { } address ||
            detachedSentence is not { } current) return;
        changingDetachedAnnotation = true;
        UpdateDetachedControls();
        try
        {
            var updated = await AppState.Session.UpdateDetachedPdfAnnotationAsync(
                address.Document, address.Page, current.Sentence.Id, new { kind = "bookmark", saved = !current.Bookmarked });
            if (ReferenceEquals(detachedAddress, address) && detachedSentence?.Sentence.Id == current.Sentence.Id)
                detachedSentence = updated;
        }
        catch (Exception error) { await DisplayAlertAsync("Закладка", error.Message, "Гаразд"); }
        finally
        {
            changingDetachedAnnotation = false;
            UpdateDetachedControls();
        }
    }

    private async void DetachedNoteClicked(object? sender, EventArgs args)
    {
        if (!DetachedNote.IsEnabled || detachedAddress is not { } address || detachedSentence is not { } current) return;
        try
        {
            await ShowNoteEditorAsync(current.Sentence, current.Note, async text =>
            {
                var updated = await AppState.Session.UpdateDetachedPdfAnnotationAsync(
                    address.Document, address.Page, current.Sentence.Id, new { kind = "note", text });
                if (ReferenceEquals(detachedAddress, address) && detachedSentence?.Sentence.Id == current.Sentence.Id)
                {
                    detachedSentence = updated;
                    UpdateDetachedControls();
                }
            });
        }
        catch (Exception error) { await DisplayAlertAsync("Нотатка", error.Message, "Гаразд"); }
    }

    private async void DetachedAnnotationsClicked(object? sender, EventArgs args)
    {
        if (!DetachedAnnotations.IsEnabled || detachedAddress is not { } address) return;
        try
        {
            var marks = await AppState.Session.DetachedPdfAnnotationsAsync(address.Document, address.Page);
            if (!ReferenceEquals(detachedAddress, address)) return;
            if (marks.Length == 0)
            {
                await DisplayAlertAsync("Позначки на сторінці", "Закладок, нотаток і власних перекладів ще немає.", "Гаразд");
                return;
            }
            var choices = marks.ToDictionary(item => BookmarkLabel(item.Position, item.Note ?? item.Text));
            var selected = await DisplayActionSheetAsync("Позначки на сторінці", "Скасувати", null, choices.Keys.ToArray());
            if (selected is not null && choices.TryGetValue(selected, out var mark) && ReferenceEquals(detachedAddress, address))
                await LoadDetachedSentenceAsync(address, mark.Position);
        }
        catch (Exception error) { await DisplayAlertAsync("Позначки", error.Message, "Гаразд"); }
    }

    private string? ReviewedDetachedTranslation() =>
        TargetCode is { } target ? detachedSentence?.ReviewedTranslations.GetValueOrDefault(target) : null;

    private string? CurrentDetachedTranslation() => ReviewedDetachedTranslation() ?? detachedMachineTranslation?.Text;

    private async void DetachedEditTranslationClicked(object? sender, EventArgs args)
    {
        if (!DetachedEditTranslation.IsEnabled || detachedAddress is not { } address ||
            detachedSentence is not { } current || TargetCode is not { } target) return;
        changingDetachedAnnotation = true;
        UpdateDetachedControls();
        try
        {
            var own = ReviewedDetachedTranslation();
            var message = own is null ? current.Sentence.Text
                : current.Sentence.Text + Environment.NewLine + Environment.NewLine
                    + "Щоб видалити переклад, збережіть порожнє поле.";
            var text = await DisplayPromptAsync("Переклад речення", message, accept: "Зберегти",
                cancel: "Скасувати", initialValue: own ?? (detachedMachineTranslation is { SourceText: null } result ? result.Text : ""),
                maxLength: 4000);
            if (text is null || !ReferenceEquals(detachedAddress, address) ||
                detachedSentence?.Sentence.Id != current.Sentence.Id || TargetCode != target) return;
            var updated = await AppState.Session.UpdateDetachedPdfAnnotationAsync(
                address.Document, address.Page, current.Sentence.Id, new { kind = "translation", target, text });
            if (ReferenceEquals(detachedAddress, address) && detachedSentence?.Sentence.Id == current.Sentence.Id)
            {
                detachedSentence = updated;
                ResetDetachedTranslation();
            }
        }
        catch (Exception error) { await DisplayAlertAsync("Переклад", error.Message, "Гаразд"); }
        finally
        {
            changingDetachedAnnotation = false;
            UpdateDetachedControls();
        }
    }

    private async void DetachedShareTranslationClicked(object? sender, EventArgs args)
    {
        if (sharingReaderText || detachedSentence is not { } current || TargetCode is not { } target
            || CurrentDetachedTranslation() is not { } translated) return;
        var source = ReviewedDetachedTranslation() is null ? detachedMachineTranslation?.SourceText : null;
        await ShareTranslationPairAsync(new TranslationPair(source ?? current.Sentence.Text, current.Language, translated, target));
    }

    private async void DetachedTranslateClicked(object? sender, EventArgs args) =>
        await TranslateDetachedAsync(background: false);

    private async Task TranslateDetachedAsync(bool background)
    {
        if (!DetachedTranslate.IsEnabled || detachedAddress is not { } address ||
            detachedSentence is not { } current || TargetCode is not { } target) return;
        var run = ++detachedTranslationRun;
        translatingDetachedSentence = true;
        UpdateDetachedControls();
        ShowDetachedTranslation("Перекладаємо офлайн…");
        try
        {
            var translated = await AppState.Session.DetachedPdfTranslationAsync(
                address.Document, address.Page, current.Sentence.Id, target, cachedOnly: false, background: background);
            if (run != detachedTranslationRun) return;
            if (translated is null)
            {
                if (!background)
                {
                    ShowDetachedTranslation("Для цього речення переклад недоступний.");
                    RefreshReadiness((current.Language, target));
                }
                else ShowDetachedTranslation("");
            }
            else
                ShowDetachedMachineTranslation(translated, current.Language, target);
        }
        catch (Exception error)
        {
            if (run == detachedTranslationRun) ShowDetachedTranslation(error.Message);
        }
        finally
        {
            if (run == detachedTranslationRun)
            {
                translatingDetachedSentence = false;
                UpdateDetachedControls();
            }
        }
    }

    private async Task RestoreDetachedTranslationAsync()
    {
        if (!readingPageActive || loadingDetachedSentence || translatingDetachedSentence ||
            detachedAddress is not { } address || detachedSentence is not { } current || TargetCode is not { } target ||
            CurrentDetachedTranslation() is not null || detachedCacheLookupRun == detachedTranslationRun) return;
        var run = detachedTranslationRun;
        detachedCacheLookupRun = run;
        try
        {
            var cached = await AppState.Session.DetachedPdfTranslationAsync(
                address.Document, address.Page, current.Sentence.Id, target, cachedOnly: true);
            if (run != detachedTranslationRun || CurrentDetachedTranslation() is not null) return;
            if (cached is not null)
            {
                ShowDetachedMachineTranslation(cached, current.Language, target);
                UpdateDetachedControls();
            }
            else if (autoTranslate && !speakingDetachedSentence && !AppState.Session.Busy)
            {
                await Task.Delay(250);
                if (run == detachedTranslationRun && readingPageActive && autoTranslate && !speakingDetachedSentence &&
                    !AppState.Session.Busy && CurrentDetachedTranslation() is null)
                    await TranslateDetachedAsync(background: true);
            }
        }
        catch (Exception error)
        {
            if (run == detachedTranslationRun && AppState.Session.Document?.Id == address.Document)
                System.Diagnostics.Debug.WriteLine(error.Message);
        }
        finally
        {
            if (detachedCacheLookupRun == run) detachedCacheLookupRun = null;
        }
    }

    private void ResetDetachedTranslation()
    {
        detachedTranslationRun++;
        translatingDetachedSentence = false;
        detachedMachineTranslation = null;
        ShowDetachedReviewedTranslation();
        UpdateDetachedControls();
        _ = RestoreDetachedTranslationAsync();
    }

    private void ShowDetachedReviewedTranslation()
    {
        var reviewed = loadingDetachedSentence ? null : ReviewedDetachedTranslation();
        ShowDetachedTranslation(reviewed is null ? "" : $"Ваш переклад{Environment.NewLine}{reviewed}");
    }

    private void ShowDetachedMachineTranslation(DetachedPdfTranslation translation, string source, string target)
    {
        detachedMachineTranslation = translation;
        var label = translation.SourceText is null ? "Машинний переклад" : "Машинний переклад групи речень";
        if (translation.PivotLanguage is { } pivot)
            label += $" · {DocumentLanguages.PivotRoute(source, pivot, target)}";
        ShowDetachedTranslation($"{label}{Environment.NewLine}{translation.Text}");
    }

    private void ShowDetachedTranslation(string value)
    {
        DetachedTranslation.Text = value;
        DetachedTranslationPane.IsVisible = value.Length != 0;
    }

    private async void DetachedSpeakClicked(object? sender, EventArgs args) => await SpeakDetachedAsync(resumePlayback: false);

    private async void DetachedResumeSpeechClicked(object? sender, EventArgs args) => await SpeakDetachedAsync(resumePlayback: true);

    private async Task SpeakDetachedAsync(bool resumePlayback)
    {
        CancelDetachedSpeechTravel();
        if (loadingDetachedSentence || detachedAddress is not { } address || detachedSentence is null) return;
        if (speakingDetachedSentence)
        {
            if (detachedPaused ? detachedSpeech.Resume() : detachedSpeech.Pause())
            {
                detachedPaused = !detachedPaused;
                if (detachedPaused) checkpointDetachedSpeech?.Invoke();
                UpdateDetachedControls();
            }
            else if (!detachedPaused) ClearDetachedSpeech();
            return;
        }

        using var cancellation = new CancellationTokenSource();
        detachedAudioCancellation = cancellation;
        var token = cancellation.Token;
        speakingDetachedSentence = true;
        var advancePage = false;
        try
        {
            while (ReferenceEquals(detachedAudioCancellation, cancellation) &&
                detachedAddress == address && detachedSentence is { } sentence)
            {
                var completed = await PlayDetachedSentenceAsync(address, sentence,
                    resumePlayback ? sentence.SpeechResume : null, token);
                resumePlayback = false;
                if (!completed || !ContinueSpeech.IsChecked || !readingPageActive ||
                    !ReferenceEquals(detachedAudioCancellation, cancellation)) break;

                var next = sentence.Position + 1;
                if (next >= sentence.Total)
                {
                    advancePage = true;
                    break;
                }

                await LoadDetachedSentenceAsync(address, next, automatic: true);
                if (detachedSentence?.Position != next) break;
            }
        }
        finally
        {
            if (ReferenceEquals(detachedAudioCancellation, cancellation)) ClearDetachedSpeech();
        }

        if (advancePage && readingPageActive && detachedAddress == address)
            await ContinuePdfNarrationAsync(address.Document, address.Page);
    }

    private async Task<bool> PlayDetachedSentenceAsync(DetachedAddress address, DetachedPdfSentence current,
        SpeechResume? saved, CancellationToken token)
    {
        var run = ++detachedAudioRun;
        var ratePercent = SpeechRatePercent;
        var rate = ratePercent / 100f;
        detachedSpeech.Rate = rate;
        UpdateDetachedControls();
        var session = AppState.Session;
        var sentence = current.Sentence;
        var speechText = current.SpeechText ?? sentence.Text;
        var hash = SpeechProgress.TextHash(sentence, speechText);
        SpeechResume? pending = null;
        IDispatcherTimer? timer = null;
        var lastSavedAt = Environment.TickCount64 - 2000;

        bool Save(SpeechResume? resume)
        {
            try
            {
                session.SetDetachedPdfSpeechResume(address.Document, address.Page, sentence.Id, resume);
                if (detachedAddress == address && detachedSentence is { } loaded && loaded.Sentence.Id == sentence.Id)
                    detachedSentence = loaded with { SpeechResume = resume };
                pending = null;
                lastSavedAt = Environment.TickCount64;
                return true;
            }
            catch (Exception error)
            {
                if (detachedAddress == address) DetachedSentenceStatus.Text = error.Message;
                return false;
            }
        }

        try
        {
            await loadingVoices.WaitAsync(token);
            if (run != detachedAudioRun) return false;
            var voice = SelectedVoice();
            string? status;
            if (voice?.Engine is { } engine)
            {
                var source = new SpeechSource(address.Document, sentence.Id, current.Language, sentence.Text, speechText);
                var key = new ClipKey(source, engine, voice.Speaker, rate);
                var startAtMs = saved is not null && SpeechProgress.Matches(sentence, saved, speechText)
                    ? ModelResumePosition(saved, voice, current.Language, ratePercent) ?? 0 : 0;
                DetachedSentenceStatus.Text = "Готуємо локальне озвучення…";
                var audio = await PrepareAudio(session, key, token, address.Page).WaitAsync(token);
                if (run != detachedAudioRun) return false;
                DetachedSentenceStatus.Text = $"Речення {current.Position + 1} із {current.Total}";
                status = await detachedSpeech.PlayWaveAsync(audio, startAtMs, actualStartMs =>
                {
                    if (run != detachedAudioRun || token.IsCancellationRequested) return;
                    var lastPosition = actualStartMs;
                    checkpointDetachedSpeech = () =>
                    {
                        if (detachedSpeech.WavePosition is not { } elapsed) return;
                        var position = actualStartMs + (int)elapsed.TotalMilliseconds;
                        if (position <= lastPosition || position >= 45_000 ||
                            (long)position * audio.SampleRate >= (long)audio.Samples * 1000) return;
                        var model = new ModelSpeechResume(voice.Id, voice.Revision!, current.Language, ratePercent, position);
                        if (Save(new SpeechResume(sentence.Id, hash, 0, model))) lastPosition = position;
                    };
                    timer = Dispatcher.CreateTimer();
                    timer.Interval = TimeSpan.FromSeconds(2);
                    timer.Tick += (_, _) => { if (run == detachedAudioRun) checkpointDetachedSpeech?.Invoke(); };
                    timer.Start();
                    if (ContinueSpeech.IsChecked)
                        _ = PrefetchNextAudioAsync(session, key, token, address.Page);
                });
            }
            else
            {
                detachedSpeech.PreferredVoice = voice?.Id == SystemDefaultVoice ? null : voice?.Id;
                var pronounced = await PrepareSystemSpeechAsync(session, current.Language, speechText);
                if (run != detachedAudioRun) return false;
                if (speechText != sentence.Text)
                    status = await detachedSpeech.SpeakAsync(pronounced, current.Language);
                else
                    status = await PlayDefaultDetachedPronunciationAsync(pronounced);
            }
            if (run != detachedAudioRun) return false;
            if (status == LocalSpeech.CompletedStatus)
            {
                checkpointDetachedSpeech = null;
                Save(null);
                return true;
            }
            if (status is not null) DetachedSentenceStatus.Text = status;
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) {}
        catch (Exception error)
        {
            if (run == detachedAudioRun) DetachedSentenceStatus.Text = error.Message;
        }
        finally
        {
            timer?.Stop();
            if (run == detachedAudioRun)
            {
                checkpointDetachedSpeech?.Invoke();
                checkpointDetachedSpeech = null;
                detachedAudioRun++;
                DetachedPdfText.SpokenWord = null;
            }
        }
        return false;

        Task<string?> PlayDefaultDetachedPronunciationAsync(string pronounced)
        {
            var startAt = SpeechProgress.ResumeOffset(sentence, saved) ?? 0;
            checkpointDetachedSpeech = () => { if (pending is { } resume) Save(resume); };
            var input = SourceRanges.ProjectPronunciation(sentence.Text, pronounced, startAt, (start, length) =>
                MainThread.BeginInvokeOnMainThread(() =>
                {
                    if (run != detachedAudioRun || detachedPaused || start < 0 || length <= 0 ||
                        start > sentence.Text.Length - length) return;
                    DetachedPdfText.SpokenWord = new TextSelection(
                        sentence.StartUtf16 + start, length, sentence.Text.Substring(start, length));
                    pending = new SpeechResume(sentence.Id, hash, start);
                    if (Environment.TickCount64 - lastSavedAt >= 2000) checkpointDetachedSpeech?.Invoke();
                }));
            return detachedSpeech.SpeakAsync(input.Text, current.Language, input.Boundary);
        }
    }

    private void CancelDetachedSpeechTravel()
    {
        var running = detachedSpeechTravel;
        detachedSpeechTravel = null;
        running?.Cancel();
    }

    private void ClearDetachedSpeech()
    {
        checkpointDetachedSpeech?.Invoke();
        checkpointDetachedSpeech = null;
        detachedAudioRun++;
        var running = detachedAudioCancellation;
        detachedAudioCancellation = null;
        running?.Cancel();
        detachedSpeech.Stop();
        speakingDetachedSentence = detachedPaused = false;
        DetachedPdfText.SpokenWord = null;
        UpdateDetachedControls();
    }

    private bool CanResumeDetachedSpeech()
    {
        if (detachedSentence is not { SpeechResume: { } resume } current ||
            !SpeechProgress.Matches(current.Sentence, resume, current.SpeechText)) return false;

        var voice = SelectedVoice();
        return voice?.Engine is not null
            ? ModelResumePosition(resume, voice, current.Language, SpeechRatePercent).HasValue
            : SpeechProgress.ResumeOffset(current.Sentence, resume).HasValue;
    }

    private void UpdateDetachedControls()
    {
        var available = !loadingDetachedSentence && detachedSentence is not null;
        DetachedPrevious.IsEnabled = available && detachedSentence is { Position: > 0 };
        DetachedNext.IsEnabled = available && detachedSentence is { } current && current.Position + 1 < current.Total;
        DetachedSpeak.IsEnabled = available;
        DetachedEditSpeechText.IsEnabled = available && !AppState.Session.Busy;
        DetachedStopSpeech.IsVisible = speakingDetachedSentence;
        DetachedResumeSpeech.IsVisible = available && !speakingDetachedSentence && CanResumeDetachedSpeech();
        DetachedAnalyze.IsEnabled = available;
        DetachedBookmark.IsEnabled = DetachedNote.IsEnabled = DetachedAnnotations.IsEnabled =
            available && !changingDetachedAnnotation;
        DetachedBookmark.Text = detachedSentence?.Bookmarked == true ? "Зняти закладку" : "Закладка";
        DetachedNote.Text = detachedSentence?.Note is null ? "Нотатка…" : "Редагувати нотатку…";
        DetachedAnalyzeSelection.IsVisible = DetachedAnalyzeSelection.IsEnabled =
            !loadingDetachedSentence && detachedSelection is not null;
        DetachedTranslateSelection.IsVisible = DetachedAnalyzeSelection.IsVisible && TargetCode is not null;
        UpdateDetachedExplainButton();
        DetachedSaveWord.IsVisible = DetachedAnalyzeSelection.IsVisible;
        DetachedSaveWord.IsEnabled = DetachedSaveWord.IsVisible && !savingDetachedWord
            && !AppState.Session.Busy && TargetCode is not null;
        var routeSelected = detachedSentence is { } loaded && TargetCode is { } target
            && readinessPair == (loaded.Language, target);
        DetachedTranslate.IsEnabled = available && !translatingDetachedSentence && routeSelected
            && readiness?.Ready == true && ReviewedDetachedTranslation() is null;
        DetachedReadiness.IsVisible = available && routeSelected && !translatingDetachedSentence
            && CurrentDetachedTranslation() is null;
        if (DetachedReadiness.IsVisible) DetachedReadiness.Text = ReadinessMessage();
        DetachedOpenTranslationPackages.IsVisible = DetachedReadiness.IsVisible && readiness is { Missing.Length: > 0 };
        DetachedEditTranslation.IsEnabled = available && !changingDetachedAnnotation && TargetCode is not null;
        DetachedEditTranslation.Text = ReviewedDetachedTranslation() is null ? "Свій переклад…" : "Редагувати переклад…";
        DetachedShareTranslation.IsVisible = available && !translatingDetachedSentence && CurrentDetachedTranslation() is not null;
        var playing = speakingDetachedSentence && !detachedPaused;
        DetachedSpeak.Text = playing ? "Ⅱ" : "▶";
        SemanticProperties.SetDescription(DetachedSpeak, playing ? "Призупинити озвучення" : "Озвучити речення сторінки");
    }
}
