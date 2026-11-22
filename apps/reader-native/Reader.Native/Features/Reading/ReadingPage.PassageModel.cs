using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record ModelPassage(string Document, int? Page, string Sentence, string Revision,
        string SourceLanguage, string Target);
    private ModelPassage? runningPassage;
    private (ModelPassage Address, ModelPassageTranslation Translation)? modelPassageResult;
    private ModelPassage? inspectingModelSource;
    private ModelPassage? attemptedModelPassage;
    private (ModelPassage Address, string Text)? modelPassageError;

    private ModelPassage? CurrentModelPassage()
    {
        var session = AppState.Session;
        if (TargetCode is not { } target || session.Document is not { } document) return null;
        if (DetachedPdfReader.IsVisible)
            return detachedAddress is { } address && detachedSentence is { RevisionId: { } revision } current
                ? new ModelPassage(document.Id, address.Page, current.Sentence.Id, revision, address.Language, target)
                : null;
        return !PdfPageNotice.IsVisible && session.Sentence is { } sentence && document.CurrentRevisionId is { } canonical
            ? new ModelPassage(document.Id, null, sentence.Id, canonical, document.Language, target)
            : null;
    }

    private async void TranslateWithModelClicked(object? sender, EventArgs args)
    {
        if (runningPassage is not null || CurrentModelPassage() is not { } address) return;
        var session = AppState.Session;
        var regenerate = attemptedModelPassage == address;
        attemptedModelPassage = address;
        modelPassageError = null;
        runningPassage = address;
        UpdateModelTranslation();
        try
        {
            var result = await session.TranslateWithModelAsync(address.Document, address.Page, address.Sentence, address.Target, regenerate);
            modelPassageResult = (address, result);
        }
        catch (Exception error)
        {
            modelPassageError = (address, error.Message);
        }
        finally
        {
            runningPassage = null;
            UpdateModelTranslation();
        }
    }

    private void UpdateModelTranslation()
    {
        var address = CurrentModelPassage();
        var detached = address?.Page is not null;
        var enabled = address is not null && runningPassage is null && !AppState.Session.Busy;
        TranslateWithModel.IsEnabled = enabled && !detached;
        DetachedTranslateWithModel.IsEnabled = enabled && detached;
        TranslateWithModel.Text = DetachedTranslateWithModel.Text = address is not null && attemptedModelPassage == address
            ? "Перегенерувати" : "Перекласти моделлю";

        var running = address is not null && runningPassage == address;
        var translation = CurrentModelTranslation(address);
        var result = translation is null ? null :
            $"{(translation.Cached ? "Збережений переклад Qwen" : "Переклад Qwen 2B")}{Environment.NewLine}{translation.Text}";
        if (modelPassageError is { } failure && failure.Address == address)
            result = result is null ? failure.Text : $"{result}{Environment.NewLine}{failure.Text}";
        ModelTranslationBusy.IsRunning = ModelTranslationBusy.IsVisible = running && !detached;
        DetachedModelTranslationBusy.IsRunning = DetachedModelTranslationBusy.IsVisible = running && detached;
        ModelTranslationResult.IsVisible = !detached && (running || result is not null);
        DetachedModelTranslationResult.IsVisible = detached && (running || result is not null);
        var text = running ? "Qwen перекладає абзац…" : result ?? "";
        if (detached) DetachedModelTranslationText.Text = text;
        else ModelTranslationText.Text = text;

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
    }

    private ModelPassageTranslation? CurrentModelTranslation(ModelPassage? address) =>
        modelPassageResult is { } saved && saved.Address == address ? saved.Translation : null;

    private void ToggleModelSourceClicked(object? sender, EventArgs args)
    {
        var address = CurrentModelPassage();
        if (CurrentModelTranslation(address)?.SourceText is null) return;
        inspectingModelSource = inspectingModelSource == address ? null : address;
        UpdateModelTranslation();
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
