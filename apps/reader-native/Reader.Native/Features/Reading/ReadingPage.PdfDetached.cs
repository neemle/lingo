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
    private string? detachedMachineTranslation;
    private bool speakingDetachedSentence;
    private bool detachedPaused;
    private int detachedAudioRun;
    private CancellationTokenSource? detachedAudioCancellation;
    private int detachedTranslationRun;

    private void ShowDetachedPdfPage(Document document, PdfReadPage page)
    {
        TargetLanguage.IsEnabled = !AppState.Session.Busy;
        var address = new DetachedAddress(document.Id, page.Page, page.Text, document.Language);
        if (detachedAddress == address) return;

        StopDetachedPdfReader();
        detachedAddress = address;
        _ = LoadDetachedSentenceAsync(address, position: null);
    }

    private void StopDetachedPdfReader()
    {
        if (detachedAddress is null) return;
        detachedAddress = null;
        detachedSentence = null;
        detachedSelection = null;
        loadingDetachedSentence = false;
        ClearDetachedSpeech();
        ResetDetachedTranslation();
    }

    private async Task LoadDetachedSentenceAsync(DetachedAddress address, int? position, string? sentenceId = null)
    {
        var request = ++detachedSentenceRequest;
        loadingDetachedSentence = true;
        detachedSelection = null;
        ClearDetachedSpeech();
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
                ShowDetachedReviewedTranslation();
                UpdateDetachedControls();
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
        detachedSelection = selected;
        UpdateDetachedControls();
    }

    private async void DetachedAnalyzeClicked(object? sender, EventArgs args)
    {
        if (!DetachedAnalyze.IsEnabled || detachedAddress is not { } address || detachedSentence is not { } current) return;
        var session = AppState.Session;
        await ShowAnalysisPageAsync("Аналіз речення", current.Sentence.Text,
            start => session.AnalyzeDetachedPdfSentenceAsync(address.Document, address.Page, current.Sentence.Id, start));
    }

    private async void DetachedAnalyzeSelectionClicked(object? sender, EventArgs args)
    {
        if (!DetachedAnalyzeSelection.IsEnabled || detachedAddress is not { } address || detachedSelection is not { } selected) return;
        var session = AppState.Session;
        await ShowAnalysisPageAsync("Аналіз виділення", selected.Text,
            _ => session.AnalyzeDetachedPdfSelectionAsync(address.Document, address.Page, selected));
    }

    private async void DetachedSaveWordClicked(object? sender, EventArgs args)
    {
        if (!DetachedSaveWord.IsEnabled || detachedAddress is not { } address ||
            detachedSelection is not { } selected || TargetCode is not { } target) return;

        var session = AppState.Session;
        savingDetachedWord = true;
        UpdateDetachedControls();
        try
        {
            var translation = await DisplayPromptAsync("Додати до словника",
                $"«{selected.Text}» · {DocumentLanguages.Name(target)}", accept: "Далі", cancel: "Скасувати",
                placeholder: "Переклад", maxLength: 2000);
            if (string.IsNullOrWhiteSpace(translation) || detachedAddress != address || TargetCode != target) return;

            var reading = new VocabularyReadingOption("Як у тексті");
            LinguisticAnalysis? details = null;
            try { details = await session.AnalyzeDetachedPdfSelectionAsync(address.Document, address.Page, selected); }
            catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
            if (detachedAddress != address || TargetCode != target) return;
            var options = DetachedVocabularyReadings(details, selected, reading);
            if (options.Length > 1)
            {
                var labels = options.Select((option, index) => $"{index + 1}. {option.Label}").ToArray();
                var choice = await DisplayActionSheetAsync("Як зберегти картку", "Скасувати", null, labels);
                var index = Array.IndexOf(labels, choice);
                if (index < 0 || detachedAddress != address || TargetCode != target) return;
                reading = options[index];
            }

            await session.WaitUntilIdleAsync();
            if (session.Document?.Id != address.Document || detachedAddress != address) return;
            await session.SaveDetachedPdfWordAsync(address.Document, address.Page, selected, translation,
                target, reading.Lemma, reading.Expression);
            if (session.Error.Length > 0)
            {
                await DisplayAlertAsync("Словник", session.Error, "Гаразд");
                return;
            }
            if (detachedAddress == address) DetachedSentenceStatus.Text = "Картку збережено в словнику.";
        }
        catch (Exception error) { await DisplayAlertAsync("Словник", error.Message, "Гаразд"); }
        finally
        {
            savingDetachedWord = false;
            UpdateDetachedControls();
        }
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

    private string? CurrentDetachedTranslation() => ReviewedDetachedTranslation() ?? detachedMachineTranslation;

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
                cancel: "Скасувати", initialValue: CurrentDetachedTranslation() ?? "", maxLength: 4000);
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
        await ShareTranslationPairAsync(new TranslationPair(current.Sentence.Text, current.Language, translated, target));
    }

    private async void DetachedTranslateClicked(object? sender, EventArgs args)
    {
        if (!DetachedTranslate.IsEnabled || detachedAddress is not { } address ||
            detachedSentence is not { } current || TargetCode is not { } target) return;
        var run = ++detachedTranslationRun;
        translatingDetachedSentence = true;
        UpdateDetachedControls();
        ShowDetachedTranslation("Перекладаємо офлайн…");
        try
        {
            var translated = await AppState.Session.TranslateDetachedPdfSentenceAsync(
                address.Document, address.Page, current.Sentence.Id, target);
            if (run != detachedTranslationRun) return;
            detachedMachineTranslation = translated?.Text;
            var text = translated?.Text ?? "Для цього напрямку потрібен пакет офлайн-перекладу.";
            if (translated?.PivotLanguage is { } pivot)
                text = $"{DocumentLanguages.PivotRoute(current.Language, pivot, target)}\n{text}";
            ShowDetachedTranslation(text);
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

    private void ResetDetachedTranslation()
    {
        detachedTranslationRun++;
        translatingDetachedSentence = false;
        detachedMachineTranslation = null;
        ShowDetachedReviewedTranslation();
        UpdateDetachedControls();
    }

    private void ShowDetachedReviewedTranslation()
    {
        var reviewed = loadingDetachedSentence ? null : ReviewedDetachedTranslation();
        ShowDetachedTranslation(reviewed is null ? "" : $"Ваш переклад{Environment.NewLine}{reviewed}");
    }

    private void ShowDetachedTranslation(string value)
    {
        DetachedTranslation.Text = value;
        DetachedTranslationPane.IsVisible = value.Length != 0;
    }

    private async void DetachedSpeakClicked(object? sender, EventArgs args)
    {
        if (loadingDetachedSentence || detachedAddress is not { } address || detachedSentence is not { } current) return;
        if (speakingDetachedSentence)
        {
            if (detachedPaused ? detachedSpeech.Resume() : detachedSpeech.Pause())
            {
                detachedPaused = !detachedPaused;
                UpdateDetachedControls();
            }
            else if (!detachedPaused) ClearDetachedSpeech();
            return;
        }

        using var cancellation = new CancellationTokenSource();
        detachedAudioCancellation = cancellation;
        var token = cancellation.Token;
        var run = ++detachedAudioRun;
        speakingDetachedSentence = true;
        var rate = SpeechRatePercent / 100f;
        detachedSpeech.Rate = rate;
        UpdateDetachedControls();
        var sentence = current.Sentence;
        try
        {
            await loadingVoices.WaitAsync(token);
            if (run != detachedAudioRun) return;
            var voice = SelectedVoice();
            if (voice?.Engine is { } engine)
            {
                var session = AppState.Session;
                var source = new SpeechSource(address.Document, sentence.Id, current.Language, sentence.Text, sentence.Text);
                var key = new ClipKey(source, engine, voice.Speaker, rate);
                DetachedSentenceStatus.Text = "Готуємо локальне озвучення…";
                var audio = await PrepareAudio(session, key, token, address.Page).WaitAsync(token);
                if (run != detachedAudioRun) return;
                DetachedSentenceStatus.Text = $"Речення {current.Position + 1} із {current.Total}";
                await detachedSpeech.PlayWaveAsync(audio, 0, startedAt =>
                {
                    if (!token.IsCancellationRequested && ContinueSpeech.IsChecked)
                        _ = PrefetchNextAudioAsync(session, key, token, address.Page);
                });
                return;
            }

            detachedSpeech.PreferredVoice = voice?.Id == SystemDefaultVoice ? null : voice?.Id;
            await detachedSpeech.SpeakAsync(sentence.Text, current.Language, (start, length) =>
                MainThread.BeginInvokeOnMainThread(() =>
                {
                    if (run != detachedAudioRun || start < 0 || length <= 0 || start > sentence.Text.Length - length) return;
                    DetachedPdfText.SpokenWord = new TextSelection(
                        sentence.StartUtf16 + start, length, sentence.Text.Substring(start, length));
                }));
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) {}
        catch (Exception error)
        {
            if (run == detachedAudioRun) DetachedSentenceStatus.Text = error.Message;
        }
        finally
        {
            if (run == detachedAudioRun) ClearDetachedSpeech();
        }
    }

    private void ClearDetachedSpeech()
    {
        detachedAudioRun++;
        var running = detachedAudioCancellation;
        detachedAudioCancellation = null;
        running?.Cancel();
        detachedSpeech.Stop();
        speakingDetachedSentence = detachedPaused = false;
        DetachedPdfText.SpokenWord = null;
        UpdateDetachedControls();
    }

    private void UpdateDetachedControls()
    {
        var available = !loadingDetachedSentence && detachedSentence is not null;
        DetachedPrevious.IsEnabled = available && detachedSentence is { Position: > 0 };
        DetachedNext.IsEnabled = available && detachedSentence is { } current && current.Position + 1 < current.Total;
        DetachedSpeak.IsEnabled = available;
        DetachedAnalyze.IsEnabled = available;
        DetachedBookmark.IsEnabled = DetachedNote.IsEnabled = DetachedAnnotations.IsEnabled =
            available && !changingDetachedAnnotation;
        DetachedBookmark.Text = detachedSentence?.Bookmarked == true ? "Зняти закладку" : "Закладка";
        DetachedNote.Text = detachedSentence?.Note is null ? "Нотатка…" : "Редагувати нотатку…";
        DetachedAnalyzeSelection.IsVisible = DetachedAnalyzeSelection.IsEnabled =
            !loadingDetachedSentence && detachedSelection is not null;
        DetachedSaveWord.IsVisible = DetachedAnalyzeSelection.IsVisible;
        DetachedSaveWord.IsEnabled = DetachedSaveWord.IsVisible && !savingDetachedWord
            && !AppState.Session.Busy && TargetCode is not null;
        DetachedTranslate.IsEnabled = available && !translatingDetachedSentence && TargetCode is not null
            && ReviewedDetachedTranslation() is null;
        DetachedEditTranslation.IsEnabled = available && !changingDetachedAnnotation && TargetCode is not null;
        DetachedEditTranslation.Text = ReviewedDetachedTranslation() is null ? "Свій переклад…" : "Редагувати переклад…";
        DetachedShareTranslation.IsVisible = available && !translatingDetachedSentence && CurrentDetachedTranslation() is not null;
        var playing = speakingDetachedSentence && !detachedPaused;
        DetachedSpeak.Text = playing ? "Ⅱ" : "▶";
        SemanticProperties.SetDescription(DetachedSpeak, playing ? "Призупинити озвучення" : "Озвучити речення сторінки");
    }
}
