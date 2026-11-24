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
    private (Task Work, int Request)? detachedWordSentenceLoad;
    private int detachedWordSelectionRequest;
    private bool changingDetachedAnnotation;
    private bool savingDetachedWord;
    private bool translatingDetachedSentence;
    private DetachedPdfTranslation? detachedMachineTranslation;
    private string detachedTranslationLabel = "";
    private int detachedTranslationCacheVersion;
    private (int Run, int CacheVersion)? detachedCacheLookup;
    private (int Sentence, int Readiness)? detachedAutoAttempt;
    private bool speakingDetachedSentence;
    private bool detachedPaused;
    private int detachedAudioRun;
    private Action? checkpointDetachedSpeech;
    private CancellationTokenSource? detachedAudioCancellation;
    private CancellationTokenSource? detachedSpeechTravel;
    private int detachedTranslationRun;
    private bool detachedTranslationScrollPending;

#if MACCATALYST
    private void ShowDetachedPdfPage(Document document, PdfReadPage page)
    {
        DetachedPdfText.Blocks = page.Blocks ?? [];
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
        DetachedAddress address, int? position, string? sentenceId = null, bool automatic = false, int? sourceOffset = null)
    {
        if (!automatic)
        {
            dictionaryFocus = null;
            documentFindFocus = null;
            CancelDetachedSpeechTravel();
        }
        var request = ++detachedSentenceRequest;
        loadingDetachedSentence = true;
        SyncAnalysisPrefetch();
        SetDetachedSelection(null);
        UpdateDetachedExplainButton();
        if (!automatic) ClearDetachedSpeech();
        ResetDetachedTranslation();
        try
        {
            var result = await AppState.Session.DetachedPdfSentenceAsync(address.Document, address.Page, position, sentenceId, sourceOffset);
            if (request != detachedSentenceRequest || !ReferenceEquals(detachedAddress, address)) return;
            var previous = detachedSentence;
            detachedSentence = result;
            if (result is not { } current) return;
            if ((position.HasValue || sourceOffset.HasValue) && previous?.Position != current.Position)
                RecordReadingActivity(address.Document);
            var sentence = current.Sentence;
            DetachedPdfText.HighlightStart = sentence.StartUtf16;
            DetachedPdfText.HighlightLength = sentence.EndUtf16 - sentence.StartUtf16;
            UpdateInlinePdfHighlight();
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
        return next >= 0 && next < current.Total
            ? LoadDetachedSentenceAsync(address, next)
            : MoveToAdjacentPdfSentenceAsync(change);
    }

    private async void DetachedPdfSelected(object? sender, TextSelection selected) =>
        await ShowDetachedSelectionAsync(selected);

    private async Task ShowDetachedSelectionAsync(TextSelection selected,
        PdfWordAnchor? pdfAnchor = null)
    {
        if (detachedAddress is not { } address ||
            (loadingDetachedSentence && detachedWordSentenceLoad?.Request != detachedSentenceRequest) ||
            SourceRanges.TrimFootnoteReference(selected, DetachedPdfText.Blocks) is not { } lexical) return;
        selected = lexical;
        var selectionRequest = ++detachedWordSelectionRequest;
        if (detachedWordSentenceLoad is { } pending)
        {
            // Loading a sentence also writes its reading position. Finish the older write first.
            await pending.Work;
            if (selectionRequest != detachedWordSelectionRequest || pending.Request != detachedSentenceRequest ||
                !ReferenceEquals(detachedAddress, address)) return;
        }
        if (loadingDetachedSentence || detachedSentence?.Sentence is not { } active || selected.StartUtf16 < active.StartUtf16 ||
            selected.StartUtf16 >= active.EndUtf16)
        {
            var load = LoadDetachedSentenceAsync(address, position: null, sourceOffset: selected.StartUtf16);
            var request = detachedSentenceRequest;
            detachedWordSentenceLoad = (load, request);
            await load;
            if (detachedWordSentenceLoad?.Request == request) detachedWordSentenceLoad = null;
            if (selectionRequest != detachedWordSelectionRequest || request != detachedSentenceRequest) return;
        }
        if (ReferenceEquals(detachedAddress, address) && detachedSentence?.Sentence is { } hit &&
            selected.StartUtf16 >= hit.StartUtf16 && selected.StartUtf16 < hit.EndUtf16)
            await ShowDetachedWordAsync(selected, pdfAnchor);
    }

    private async Task ShowDetachedWordAsync(TextSelection selected, PdfWordAnchor? pdfAnchor)
    {
        if (detachedAddress is not { } address || detachedSentence is not { } sentence || loadingDetachedSentence) return;
        dictionaryFocus = null;
        RecordReadingActivity(address.Document);
        SetDetachedSelection(selected);
        UpdateDetachedControls();
        DetachedSelectedWord.Text = selected.ReadingText;
        ShowWordPopup(DetachedPdfText, selected, DetachedWordCard, pdfAnchor);
        await LoadDetachedSyntaxHintAsync(address, sentence.RevisionId, selected);
    }

    private void SetDetachedSelection(TextSelection? selected)
    {
        if (detachedSelection != selected)
        {
            detachedWordTranslation = null;
            ResetDetachedMeanings();
            ResetDetachedSyntax();
        }
        detachedSelection = selected;
        ResetExplanation(DetachedWordExplanationText, DetachedWordExplanationGloss);
        if (selected is null) DetachedWordCard.IsVisible = false;
    }

    private async void DetachedAnalyzeClicked(object? sender, EventArgs args) => await AnalyzeDetachedSentenceAsync();

    private async Task AnalyzeDetachedSentenceAsync()
    {
        if (!DetachedAnalyze.IsEnabled || detachedAddress is not { } address || detachedSentence is not { } current) return;
        var session = AppState.Session;
        await ShowAnalysisPageAsync("Перевірка речення", current.Sentence.Text,
            start => session.AnalyzeDetachedPdfSentenceAsync(address.Document, address.Page, current.Sentence.Id, start),
            (model, cachedOnly, cancellationToken) => session.ReviewSentenceAsync(model, address.Document, address.Page, current.Sentence.Id,
                revisionId: current.RevisionId, cachedOnly: cachedOnly, cancellationToken: cancellationToken));
    }

    private async void DetachedAnalyzeSelectionClicked(object? sender, EventArgs args)
    {
        if (!DetachedAnalyzeSelection.IsEnabled || detachedAddress is not { } address ||
            detachedSelection is not { } selected || detachedSentence is not { } current) return;
        var session = AppState.Session;
        await ShowAnalysisPageAsync("Перевірка виділення", selected.Text,
            _ => session.AnalyzeDetachedPdfSelectionAsync(address.Document, address.Page, selected),
            (model, cachedOnly, cancellationToken) => session.ReviewSentenceAsync(model, address.Document, address.Page, null,
                new { kind = "selection", startUtf16 = selected.StartUtf16, text = selected.Text },
                revisionId: current.RevisionId, cachedOnly: cachedOnly, cancellationToken: cancellationToken));
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
        var draft = CurrentPopupTranslation(detached: true);
        var reconstructed = draft is { } result && result.Source != selected.Text.Trim() ? result.Source : null;
        try
        {
            var meaning = detachedSense?.Target == target ? detachedSense : null;
            var sense = meaning?.Sense;
            if (sense is not null) reconstructed = null;
            var description = $"«{reconstructed ?? selected.Text}» · {DocumentLanguages.Name(target)}";
            var translation = await DisplayPromptAsync("Додати до словника", description,
                accept: "Далі", cancel: "Скасувати", placeholder: "Переклад",
                initialValue: meaning?.Translation ?? draft?.Text,
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
            .Select(expression => new VocabularyReadingOption($"Вираз · {expression.Text}",
                Expression: ExpressionCard(expression, package)))];
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
                detachedSentence?.Sentence.Id != current.Sentence.Id || TargetCode != target ||
                AppState.Session.Document is not { } document || document.Id != address.Document ||
                document.Language != address.Language) return;
            var reviewPage = new PdfLayoutAddress(address.Document, address.Page, document.CurrentRevisionId,
                address.Language, target, address.Text);
            ClearTranslationFocus();
            var updated = await AppState.Session.UpdateDetachedPdfAnnotationAsync(
                address.Document, address.Page, current.Sentence.Id, new { kind = "translation", target, text });
            if (own is not null && string.IsNullOrWhiteSpace(text))
                AppState.TranslationWorker?.TranslationReviewRemoved(address.Document, target);
            if (ReferenceEquals(detachedAddress, address) && detachedSentence?.Sentence.Id == current.Sentence.Id)
            {
                detachedSentence = updated;
                ResetDetachedTranslation();
            }
            InvalidateReviewedPdfRows(reviewPage, current.Sentence);
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
        if (sharingReaderText || !TranslationVisible || DisplayedDetachedTranslationPair() is not { } pair) return;
        await ShareTranslationPairAsync(pair);
    }

    private async void DetachedTranslateClicked(object? sender, EventArgs args)
    {
        ClearTranslationFocus();
        await TranslateDetachedAsync(automatic: false);
    }

    private async Task RefreshDetachedSentenceTranslationAsync()
    {
        if (detachedAddress is not { } address || detachedSentence is not { } current || TargetCode is not { } target ||
            AppState.Session.Document is not { } document || document.Id != address.Document) return;
        var refreshAddress = new PdfLayoutAddress(address.Document, address.Page, document.CurrentRevisionId,
            address.Language, target, address.Text);
        var run = ++detachedTranslationRun;
        translatingDetachedSentence = true;
        UpdateDetachedControls();
        try
        {
            var refresh = new PdfSentenceRefresh(current.Sentence.StartUtf16,
                new(current.Sentence.Id, current.Language, current.RevisionId));
            await AppState.Session.PdfPageTranslationAsync(address.Document, address.Page, target,
                cachedOnly: false, refresh: refresh);
            if (run == detachedTranslationRun) ClearTranslationFocus();
        }
        catch (Exception error)
        {
            if (run == detachedTranslationRun) await DisplayAlertAsync("Переклад речення", error.Message, "Гаразд");
            return;
        }
        finally
        {
            if (run == detachedTranslationRun)
            {
                translatingDetachedSentence = false;
                UpdateDetachedControls();
            }
        }
        NotifyPdfSentenceRefresh(refreshAddress, current.Sentence);
    }

    private async Task TranslateDetachedAsync(bool automatic)
    {
        if (!DetachedTranslate.IsEnabled || detachedAddress is not { } address ||
            detachedSentence is not { } current || TargetCode is not { } target) return;
        var run = ++detachedTranslationRun;
        var cacheVersion = detachedTranslationCacheVersion;
        translatingDetachedSentence = true;
        UpdateDetachedControls();
        if (CurrentDetachedTranslation() is null) ShowDetachedTranslation("Перекладаємо офлайн…");
        try
        {
            var translated = await AppState.Session.DetachedPdfPassageAsync(
                address.Document, address.Page, current.Sentence.Id, target, cachedOnly: false);
            if (run != detachedTranslationRun || cacheVersion != detachedTranslationCacheVersion) return;
            RenderDetachedPassage(address, current, translated.Rows);
            if (ReviewedDetachedTranslation() is not null)
                ShowDetachedReviewedTranslation();
            else if (translated.Active is not { } active)
            {
                if (!automatic)
                {
                    ShowDetachedTranslation("Для цього речення переклад недоступний.");
                    RefreshReadiness((current.Language, target));
                }
                else if (detachedMachineTranslation is null) ShowDetachedTranslation("");
            }
            else
                ShowDetachedMachineTranslation(active, current.Language, target);
        }
        catch (Exception error)
        {
            if (run == detachedTranslationRun && cacheVersion == detachedTranslationCacheVersion &&
                ReviewedDetachedTranslation() is null &&
                (!automatic || detachedMachineTranslation is null))
                ShowDetachedTranslation(error.Message);
        }
        finally
        {
            if (run == detachedTranslationRun)
            {
                translatingDetachedSentence = false;
                UpdateDetachedControls();
                if (cacheVersion != detachedTranslationCacheVersion)
                    _ = RestoreDetachedTranslationAsync(allowAutoTranslate: false);
            }
        }
    }

    private void RecheckDetachedTranslationCache()
    {
        detachedTranslationCacheVersion++;
        _ = RestoreDetachedTranslationAsync(allowAutoTranslate: false);
    }

    private async Task RestoreDetachedTranslationAsync(bool allowAutoTranslate = true)
    {
        if (!readingPageActive || loadingDetachedSentence || translatingDetachedSentence ||
            detachedAddress is not { } address || detachedSentence is not { Footnote: false } current || TargetCode is not { } target)
            return;
        var run = detachedTranslationRun;
        var cacheVersion = detachedTranslationCacheVersion;
        var request = (run, cacheVersion);
        if (detachedCacheLookup == request) return;
        detachedCacheLookup = request;
        try
        {
            var cached = await AppState.Session.DetachedPdfPassageAsync(
                address.Document, address.Page, current.Sentence.Id, target, cachedOnly: true);
            if (run != detachedTranslationRun || cacheVersion != detachedTranslationCacheVersion) return;
            RenderDetachedPassage(address, current, cached.Rows);
            if (cached.Active is { } active && ReviewedDetachedTranslation() is null)
            {
                ShowDetachedMachineTranslation(active, current.Language, target);
                UpdateDetachedControls();
            }
        }
        catch (Exception error)
        {
            if (run == detachedTranslationRun && AppState.Session.Document?.Id == address.Document)
                System.Diagnostics.Debug.WriteLine(error.Message);
        }
        finally
        {
            if (detachedCacheLookup == request) detachedCacheLookup = null;
        }
        if (!allowAutoTranslate) return;
        var attempt = (detachedSentenceRequest, readinessRequest);
        bool CanRefresh() => detachedAutoAttempt != attempt && run == detachedTranslationRun &&
            cacheVersion == detachedTranslationCacheVersion &&
            readingPageActive && autoTranslate && !PdfUsesPageTranslation && !speakingDetachedSentence &&
            DetachedTranslate.IsEnabled && readinessPair == (current.Language, target) &&
            readiness is { Ready: true } preferred && ReviewedDetachedTranslation() is null &&
            (detachedMachineTranslation is not { } machine || machine.LegacyGroup ||
                machine.PivotLanguage != preferred.PivotLanguage || machine.DirectModelRevision != preferred.DirectModelRevision);
        if (!CanRefresh()) return;
        await Task.Delay(250);
        if (attempt != (detachedSentenceRequest, readinessRequest) || !CanRefresh()) return;
        detachedAutoAttempt = attempt;
        await TranslateDetachedAsync(automatic: true);
    }

    private void ResetDetachedTranslation()
    {
        detachedTranslationRun++;
        translatingDetachedSentence = false;
        detachedMachineTranslation = null;
        ClearDetachedPassageRows();
        ShowDetachedReviewedTranslation();
        UpdateDetachedControls();
        _ = RestoreDetachedTranslationAsync();
    }

    private void ShowDetachedReviewedTranslation()
    {
        var reviewed = loadingDetachedSentence || detachedSentence?.Footnote == true ? null : ReviewedDetachedTranslation();
        ShowDetachedTranslation(reviewed is null ? "" : $"Ваш переклад{Environment.NewLine}{reviewed}");
    }

    private void ShowDetachedMachineTranslation(DetachedPdfTranslation translation, string source, string target)
    {
        detachedMachineTranslation = translation;
        var label = translation.SourceText is null ? "Машинний переклад" : "Машинний переклад групи речень";
        if (translation.LegacyGroup) label = $"Збережено раніше · {label}";
        label = DocumentLanguages.WithPivotRoute(label, source, translation.PivotLanguage, target);
        ShowDetachedTranslation($"{label}{Environment.NewLine}{translation.Text}");
    }

    private void ShowDetachedTranslation(string value)
    {
        detachedTranslationLabel = value;
        RenderDetachedTranslation();
    }

    private void RenderDetachedTranslation()
    {
        var hit = FoundTranslation(detached: true);
        var value = hit is null ? detachedTranslationLabel
            : DocumentLanguages.WithPivotRoute("Збіг у збереженому перекладі",
                hit.SourceLanguage, hit.PivotLanguage, hit.TargetLanguage) + Environment.NewLine + hit.Target;
        var changed = DetachedTranslation.Text != value;
        DetachedTranslation.Text = value;
        if (!changed && documentFindFocus?.Translation is not null)
            HighlightActiveTranslation(detached: true, DetachedWordCard.IsVisible ? PopupGloss(detached: true) : null);
        DetachedTranslationPane.IsVisible = value.Length != 0;
        if (DetachedTranslationPane.IsVisible) FollowDetachedTranslation();
        if (changed && CurrentDetachedTranslation() is not null) RefreshCachedPdfPageLayout();
    }

    private void ClearDetachedPassageRows()
    {
        SuppressTranslationViewportSync();
        detachedPreviewRows.Clear();
        DetachedPassageBefore.Children.Clear();
        DetachedPassageAfter.Children.Clear();
        DetachedPassageBefore.IsVisible = DetachedPassageAfter.IsVisible = false;
    }

    private void RenderDetachedPassage(DetachedAddress address, DetachedPdfSentence active, DetachedPdfPassageRow[] rows)
    {
        ClearDetachedPassageRows();
        foreach (var row in rows)
        {
            if (active.Position >= row.Position && active.Position < row.Position + row.SentenceCount) continue;
            var label = new Label
            {
                Text = row.Text, FontFamily = "Georgia", FontSize = Math.Max(16, SourceText.ReadingFontSize - 4),
                LineHeight = 1.4, TextColor = Color.FromArgb("#655F4D"), Margin = new Thickness(12, 0),
            };
            var select = new TapGestureRecognizer();
            select.Tapped += async (_, _) =>
            {
                if (ReferenceEquals(detachedAddress, address) && !loadingDetachedSentence && !AppState.Session.Busy)
                    await LoadDetachedSentenceAsync(address, row.Position);
            };
            label.GestureRecognizers.Add(select);
            var pane = row.Position < active.Position ? DetachedPassageBefore : DetachedPassageAfter;
            pane.Children.Add(label);
            pane.IsVisible = true;
            detachedPreviewRows.Add(new(row.Position, label));
        }
        if (DetachedTranslationPane.IsVisible) FollowDetachedTranslation();
    }

    private void DetachedTranslationViewportReady(object? sender, EventArgs args)
    {
        if (DetachedTranslationPane.IsVisible) FollowDetachedTranslation();
    }

    private void FollowDetachedTranslation()
    {
        if (detachedTranslationScrollPending || !DetachedTranslationViewport.IsVisible ||
            DetachedTranslationViewport.Height <= 0) return;
        detachedTranslationScrollPending = true;
        if (!Dispatcher.Dispatch(async () =>
        {
            detachedTranslationScrollPending = false;
            if (!DetachedPdfReader.IsVisible || !DetachedTranslationPane.IsVisible) return;
            var viewport = DetachedTranslationViewport;
            var top = viewport.ScrollY;
            if (DetachedTranslationPane.Y >= top && DetachedTranslationPane.Y + DetachedTranslationPane.Height <= top + viewport.Height)
                return;
            SuppressTranslationViewportSync();
            try { await viewport.ScrollToAsync(DetachedTranslationPane, ScrollToPosition.MakeVisible, animated: false); }
            catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
        })) detachedTranslationScrollPending = false;
    }

    private async void DetachedSpeakClicked(object? sender, EventArgs args) => await SpeakDetachedAsync(resumePlayback: true);

    private async Task SpeakDetachedAsync(bool resumePlayback, bool singleSentence = false)
    {
        CancelDetachedSpeechTravel();
        if (loadingDetachedSentence || detachedAddress is not { } address || detachedSentence is null) return;
        ClearTranslationFocus();
        StopWordDetail();
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
                if (sentence.Position == 0 || sentence.Position + 1 == sentence.Total)
                {
                    var latest = await AppState.Session.DetachedPdfSentenceAsync(
                        address.Document, address.Page, position: null, sentenceId: sentence.Sentence.Id).WaitAsync(token);
                    if (!ReferenceEquals(detachedAudioCancellation, cancellation) ||
                        detachedAddress != address || !ReferenceEquals(detachedSentence, sentence) ||
                        latest is null || latest.Position != sentence.Position) break;
                    detachedSentence = sentence = latest;
                }
                var speakable = !sentence.Footnote && HasNarrationText(
                    sentence.SpeechText ?? sentence.NarrationText ?? sentence.Sentence.Text);
                if (!speakable && (singleSentence || !ContinueSpeech.IsChecked))
                {
                    DetachedSentenceStatus.Text = sentence.Footnote ? SkippedFootnote : EmptyNarration;
                    break;
                }
                var completed = !speakable || await PlayDetachedSentenceAsync(address, sentence,
                    resumePlayback ? sentence.SpeechResume : null, singleSentence, token);
                resumePlayback = false;
                if (!completed || singleSentence || !ContinueSpeech.IsChecked || !readingPageActive ||
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
        catch (OperationCanceledException) when (token.IsCancellationRequested) {}
        catch (Exception error)
        {
            if (ReferenceEquals(detachedAudioCancellation, cancellation)) DetachedSentenceStatus.Text = error.Message;
        }
        finally
        {
            if (ReferenceEquals(detachedAudioCancellation, cancellation)) ClearDetachedSpeech();
        }

        if (advancePage && readingPageActive && detachedAddress == address)
            await ContinuePdfNarrationAsync(address.Document, address.Page);
    }

    private async Task<bool> PlayDetachedSentenceAsync(DetachedAddress address, DetachedPdfSentence current,
        SpeechResume? saved, bool singleSentence, CancellationToken token)
    {
        var run = ++detachedAudioRun;
        var ratePercent = SpeechRatePercent;
        var rate = ratePercent / 100f;
        detachedSpeech.Rate = rate;
        UpdateDetachedControls();
        var session = AppState.Session;
        var sentence = current.Sentence;
        var speechText = current.SpeechText ?? current.NarrationText ?? sentence.Text;
        var hash = SpeechProgress.TextHash(sentence, speechText);
        var receivedCue = false;
        SpeechResume? pending = null;
        IDispatcherTimer? timer = null;
        var lastSavedAt = Environment.TickCount64 - 2000;

        bool Save(SpeechResume? resume)
        {
            try
            {
                if (resume is not null)
                    session.SetDetachedPdfSpeechResume(address.Document, address.Page, sentence.Id, resume);
                else if (!session.ClearSpeechResume(address.Document, address.Page, sentence.Id, hash, current.Language))
                    return true;
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
            if (speechText.Length == 0)
            {
                Save(null);
                return true;
            }
            RecordReadingActivity(address.Document);
            await WaitForSpeechVoicesAsync(token);
            if (run != detachedAudioRun) return false;
            var voice = SelectedVoice();
            string? status = null;
            if (voice?.Engine is { } engine)
            {
                try
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
                            if (position <= lastPosition || position >= LocalSpeech.MaxModelAudioSeconds * 1000 ||
                                (long)position * audio.SampleRate >= (long)audio.Samples * 1000) return;
                            var model = new ModelSpeechResume(voice.Id, ModelTimelineRevision(voice, current.Language), current.Language, ratePercent, position);
                            if (Save(new SpeechResume(sentence.Id, hash, 0, model))) lastPosition = position;
                        };
                        timer = Dispatcher.CreateTimer();
                        timer.Interval = TimeSpan.FromSeconds(2);
                        timer.Tick += (_, _) => { if (run == detachedAudioRun) checkpointDetachedSpeech?.Invoke(); };
                        timer.Start();
                        if (!singleSentence && ContinueSpeech.IsChecked)
                            _ = PrefetchNextAudioAsync(session, key, token, address.Page);
                    });
                }
                catch (OperationCanceledException) when (token.IsCancellationRequested) { return false; }
                catch (Exception error) { status = $"Локальний голос недоступний. {error.Message}"; }
            }
            if (run != detachedAudioRun || token.IsCancellationRequested) return false;
            var fallback = voice?.Engine is not null && status is not null && status != LocalSpeech.CompletedStatus;
            if (voice?.Engine is null || fallback)
            {
                var modelError = fallback ? status : null;
                if (fallback)
                {
                    timer?.Stop();
                    checkpointDetachedSpeech = null;
                    DetachedSentenceStatus.Text = "Локальний голос недоступний. Використовуємо системний голос.";
                }
                detachedSpeech.PreferredVoice = fallback || voice?.Id == LocalSpeech.SystemDefaultVoice ? null : voice?.Id;
                var pronunciation = current.SpeechText ?? current.PronunciationText ?? speechText;
                var pronounced = await LocalSpeech.PrepareSystemSpeechAsync(session, current.Language, pronunciation);
                if (run != detachedAudioRun) return false;
                status = await PlayDetachedPronunciationAsync(pronounced);
                if (modelError is not null && status is not null && status != LocalSpeech.CompletedStatus)
                    status = $"{modelError} {status}";
            }
            if (run != detachedAudioRun) return false;
            if (status == LocalSpeech.CompletedStatus)
            {
                RecordReadingActivity(address.Document);
                checkpointDetachedSpeech = null;
                if (Save(null))
                    DetachedSentenceStatus.Text = receivedCue ? status : $"{status} Підсвічення за реченням.";
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

        Task<string?> PlayDetachedPronunciationAsync(string pronounced)
        {
            var canSaveWordPosition = SpeechProgress.SupportsWordResume(sentence.Text, speechText);
            var startAt = SpeechProgress.ResumeOffset(sentence, saved, speechText) ?? 0;
            if (canSaveWordPosition)
                checkpointDetachedSpeech = () => { if (pending is { } resume) Save(resume); };
            var input = SourceRanges.ProjectPronunciation(sentence.Text, pronounced, startAt, (start, length) =>
                MainThread.BeginInvokeOnMainThread(() =>
                {
                    if (run != detachedAudioRun || token.IsCancellationRequested || detachedPaused ||
                        detachedAddress != address || detachedSentence is not { } frame ||
                        frame.RevisionId != current.RevisionId || frame.Sentence.Id != sentence.Id ||
                        frame.Sentence.Text != sentence.Text ||
                        SourceRanges.SpeechSelection(DetachedPdfText.Text, sentence, start, length) is not { } spoken)
                        return;
                    DetachedPdfText.SpokenWord = spoken;
                    receivedCue = true;
                    if (!canSaveWordPosition) return;
                    pending = new SpeechResume(sentence.Id, hash, spoken.StartUtf16 - sentence.StartUtf16);
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
            !SpeechProgress.Matches(current.Sentence, resume, current.SpeechText ?? current.NarrationText)) return false;

        var voice = SelectedVoice();
        return voice?.Engine is not null
            ? ModelResumePosition(resume, voice, current.Language, SpeechRatePercent).HasValue
            : SpeechProgress.ResumeOffset(current.Sentence, resume, current.SpeechText ?? current.NarrationText).HasValue;
    }

    private void UpdateDetachedControls()
    {
        UpdateSentenceMenu();
        var available = !loadingDetachedSentence && detachedSentence is not null;
        DetachedPrevious.IsEnabled = available && (detachedSentence is { Position: > 0 } || HasAdjacentPdfPage(-1));
        DetachedNext.IsEnabled = available && detachedSentence is { } current &&
            (current.Position + 1 < current.Total || HasAdjacentPdfPage(1));
        DetachedSpeak.IsEnabled = available;
        DetachedRepeatSpeech.IsEnabled = available && detachedSentence?.Footnote == false;
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
        DetachedOpenSyntax.IsVisible = DetachedAnalyzeSelection.IsVisible;
        DetachedTranslateSelection.IsVisible = DetachedAnalyzeSelection.IsVisible && TargetCode is not null;
        DetachedShareSelection.IsVisible = detachedSelection is { } selected && CanShareDetachedSelection(selected);
        UpdateDetachedExplainButton();
        DetachedSaveWord.IsVisible = DetachedAnalyzeSelection.IsVisible;
        DetachedSaveWord.IsEnabled = DetachedSaveWord.IsVisible && !savingDetachedWord
            && !AppState.Session.Busy && TargetCode is not null;
        var routeSelected = detachedSentence is { } loaded && TargetCode is { } target
            && readinessPair == (loaded.Language, target);
        DetachedTranslate.IsEnabled = available && detachedSentence?.Footnote == false && !translatingDetachedSentence && routeSelected
            && readiness?.Ready == true;
        UpdateSentenceRefreshButtons();
        DetachedReadiness.IsVisible = available && routeSelected && !translatingDetachedSentence
            && CurrentDetachedTranslation() is null;
        if (DetachedReadiness.IsVisible)
            DetachedReadiness.Text = detachedSentence?.Footnote == true ? "Виноски залишаються в оригіналі." : ReadinessMessage();
        UpdateTranslationPackageActions();
        DetachedEditTranslation.IsEnabled = available && detachedSentence?.Footnote == false && !changingDetachedAnnotation && TargetCode is not null;
        DetachedEditTranslation.Text = ReviewedDetachedTranslation() is null ? "Свій переклад…" : "Редагувати переклад…";
        UpdateModelTranslation();
        DetachedShareTranslation.IsVisible = available && !translatingDetachedSentence && DisplayedDetachedTranslationPair() is not null;
        var playing = speakingDetachedSentence && !detachedPaused;
        DetachedSpeak.Text = playing ? "Ⅱ" : "▶";
        DescribeButton(DetachedSpeak, playing ? "Призупинити озвучення"
            : detachedPaused || DetachedResumeSpeech.IsVisible ? "Продовжити озвучення" : "Озвучити речення сторінки");
    }
}
