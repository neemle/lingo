using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record ModelPassage(string Document, int? Page, string Sentence, string Target);
    private ModelPassage? runningPassage;
    private (ModelPassage Address, string Text)? modelPassageResult;
    private ModelPassage? attemptedModelPassage;
    private (ModelPassage Address, string Text)? modelPassageError;

    private ModelPassage? CurrentModelPassage()
    {
        var session = AppState.Session;
        if (TargetCode is not { } target || session.Document is not { } document) return null;
        if (DetachedPdfReader.IsVisible)
            return detachedAddress is { } address && detachedSentence is { } current
                ? new ModelPassage(document.Id, address.Page, current.Sentence.Id, target) : null;
        return !PdfPageNotice.IsVisible && session.Sentence is { } sentence
            ? new ModelPassage(document.Id, null, sentence.Id, target) : null;
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
            var label = result.Cached ? "Збережений переклад Qwen" : "Переклад Qwen 2B";
            modelPassageResult = (address, $"{label}{Environment.NewLine}{result.Text}");
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
        var result = modelPassageResult is { } saved && saved.Address == address ? saved.Text : null;
        if (modelPassageError is { } failure && failure.Address == address)
            result = result is null ? failure.Text : $"{result}{Environment.NewLine}{failure.Text}";
        ModelTranslationBusy.IsRunning = ModelTranslationBusy.IsVisible = running && !detached;
        DetachedModelTranslationBusy.IsRunning = DetachedModelTranslationBusy.IsVisible = running && detached;
        ModelTranslationResult.IsVisible = !detached && (running || result is not null);
        DetachedModelTranslationResult.IsVisible = detached && (running || result is not null);
        var text = running ? "Qwen перекладає абзац…" : result ?? "";
        if (detached) DetachedModelTranslationText.Text = text;
        else ModelTranslationText.Text = text;
    }
}
