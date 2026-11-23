using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record ModelPassage(string Document, int? Page, string Sentence, string Revision,
        string SourceLanguage, string Target);
    private ModelPassage? runningPassage;
    private CancellationTokenSource? passageCancellation;
    private (ModelPassage Address, ModelPassageTranslation Translation)? modelPassageResult;
    private ModelPassage? inspectingModelSource;
    private ModelPassage? requestedCachedPassage;
    private int cachedPassageRequest;
    private (ModelPassage Address, string Text)? modelPassageError;

    private ModelPassage? CurrentModelPassage()
    {
        var session = AppState.Session;
        if (TargetCode is not { } target || session.Document is not { } document) return null;
        if (DetachedPdfReader.IsVisible)
            return detachedAddress is { } address && detachedSentence is { Footnote: false, RevisionId: { } revision } current
                ? new ModelPassage(document.Id, address.Page, current.Sentence.Id, revision, address.Language, target)
                : null;
        return !PdfPageNotice.IsVisible && session.Sentence is { } sentence && !IsFootnote(document, sentence) &&
            document.CurrentRevisionId is { } canonical
            ? new ModelPassage(document.Id, null, sentence.Id, canonical, document.Language, target)
            : null;
    }

    private async void TranslateWithModelClicked(object? sender, EventArgs args)
    {
        if (CurrentModelPassage() is not { } address) return;
        if (runningPassage is not null)
        {
            if (runningPassage == address)
            {
                CancelModelPassage();
                UpdateModelTranslation();
            }
            return;
        }
        var session = AppState.Session;
        var regenerate = CurrentModelTranslation(address) is not null;
        cachedPassageRequest++;
        modelPassageError = null;
        runningPassage = address;
        using var cancellation = new CancellationTokenSource();
        passageCancellation = cancellation;
        UpdateModelTranslation();
        try
        {
            var result = await session.TranslateWithModelAsync(address.Document, address.Page, address.Sentence,
                address.Target, regenerate, cancellation.Token);
            if (readingPageActive && !cancellation.IsCancellationRequested && CurrentModelPassage() == address)
                modelPassageResult = (address, result);
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { }
        catch (Exception error) when (readingPageActive && CurrentModelPassage() == address)
        {
            modelPassageError = (address, error.Message);
        }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
        finally
        {
            passageCancellation = null;
            runningPassage = null;
            if (readingPageActive) UpdateModelTranslation();
        }
    }

    private void CancelModelPassage()
    {
        if (passageCancellation is { IsCancellationRequested: false } cancellation)
            _ = cancellation.CancelAsync();
    }

    private void UpdateModelTranslation()
    {
        var address = CurrentModelPassage();
        var detached = address?.Page is not null;
        if (runningPassage is not null && runningPassage != address) CancelModelPassage();
        var running = address is not null && runningPassage == address;
        var canceling = passageCancellation?.IsCancellationRequested == true;
        var enabled = running ? !canceling : address is not null && runningPassage is null && !AppState.Session.Busy;
        RestoreCachedModelTranslation(address);
        TranslateWithModel.IsEnabled = enabled && !detached;
        DetachedTranslateWithModel.IsEnabled = enabled && detached;
        var translation = CurrentModelTranslation(address);
        TranslateWithModel.Text = DetachedTranslateWithModel.Text = running ? "Скасувати переклад" : translation is not null
            ? "Перегенерувати" : address is not null && modelPassageError?.Address == address
                ? "Повторити переклад" : "Перекласти моделлю";

        var result = translation is null ? null :
            $"{(translation.Cached ? "Збережений переклад Qwen" : "Переклад Qwen 2B")}{Environment.NewLine}{translation.Text}";
        if (modelPassageError is { } failure && failure.Address == address)
            result = result is null ? failure.Text : $"{result}{Environment.NewLine}{failure.Text}";
        ModelTranslationBusy.IsRunning = ModelTranslationBusy.IsVisible = running && !detached;
        DetachedModelTranslationBusy.IsRunning = DetachedModelTranslationBusy.IsVisible = running && detached;
        ModelTranslationViewport.IsVisible = ModelTranslationResult.IsVisible = !detached && (running || result is not null);
        DetachedModelTranslationViewport.IsVisible = DetachedModelTranslationResult.IsVisible = detached && (running || result is not null);
        var text = running ? canceling ? "Скасовуємо переклад…" : "Qwen перекладає абзац…" : result ?? "";
        var label = detached ? DetachedModelTranslationText : ModelTranslationText;
        var textChanged = label.Text != text;
        label.Text = text;

        var source = translation?.SourceText;
        var hasSource = !running && !string.IsNullOrWhiteSpace(source);
        var showSource = hasSource && inspectingModelSource == address;
        ModelTranslationActions.IsVisible = hasSource && !detached;
        DetachedModelTranslationActions.IsVisible = hasSource && detached;
        ModelSourcePreview.IsVisible = showSource && !detached;
        DetachedModelSourcePreview.IsVisible = showSource && detached;
        ModelSourceToggle.Text = DetachedModelSourceToggle.Text = showSource ? "Сховати уривок" : "Що переклав Qwen";
        if (showSource)
        {
            if (detached) DetachedModelSourceText.Text = source;
            else ModelSourceText.Text = source;
        }
        if (textChanged && (running || result is not null))
        {
            var viewport = detached ? DetachedModelTranslationViewport : ModelTranslationViewport;
            if (viewport.ScrollY > 0)
                _ = RevealModelTranslationAsync(detached, detached ? DetachedModelTranslationResult : ModelTranslationResult);
        }
    }

    private void RestoreCachedModelTranslation(ModelPassage? address)
    {
        if (requestedCachedPassage == address) return;
        requestedCachedPassage = address;
        var request = ++cachedPassageRequest;
        if (address is not null && CurrentModelTranslation(address) is null)
            _ = ReadCachedModelTranslationAsync(address, request);
    }

    private async Task ReadCachedModelTranslationAsync(ModelPassage address, int request)
    {
        await Task.Delay(140);
        bool Current() => readingPageActive && request == cachedPassageRequest &&
            CurrentModelPassage() == address && runningPassage != address;
        if (!Current()) return;
        try
        {
            var translation = await AppState.Session.CachedModelTranslationAsync(
                address.Document, address.Page, address.Sentence, address.Target);
            if (translation is null || !Current()) return;
            modelPassageResult = (address, translation);
            UpdateModelTranslation();
        }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
    }

    private ModelPassageTranslation? CurrentModelTranslation(ModelPassage? address) =>
        modelPassageResult is { } saved && saved.Address == address ? saved.Translation : null;

    private async void ToggleModelSourceClicked(object? sender, EventArgs args)
    {
        var address = CurrentModelPassage();
        if (CurrentModelTranslation(address)?.SourceText is null) return;
        inspectingModelSource = inspectingModelSource == address ? null : address;
        UpdateModelTranslation();
        if (inspectingModelSource == address)
        {
            var detached = address?.Page is not null;
            await RevealModelTranslationAsync(detached, detached ? DetachedModelSourcePreview : ModelSourcePreview);
        }
    }

    private async Task RevealModelTranslationAsync(bool detached, VisualElement element)
    {
        var viewport = detached ? DetachedModelTranslationViewport : ModelTranslationViewport;
        if (!viewport.IsVisible || !element.IsVisible) return;
        try { await viewport.ScrollToAsync(element, ScrollToPosition.Start, animated: false); }
        catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
    }

    private async void ShareModelTranslationClicked(object? sender, EventArgs args)
    {
        var address = CurrentModelPassage();
        if (sharingReaderText || address is null ||
            CurrentModelTranslation(address) is not { SourceText: { Length: > 0 } original } translation)
            return;
        await ShareTranslationPairAsync(new TranslationPair(original, address.SourceLanguage, translation.Text, address.Target));
    }
}
