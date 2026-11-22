using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool explainingWord;

    private async void ExplainWordClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (explainingWord || selection is not { } word ||
            session.Document is not { CurrentRevisionId: { } revision } document) return;
        var sentence = Array.Find(document.Sentences,
            item => word.StartUtf16 >= item.StartUtf16 && word.StartUtf16 < item.EndUtf16);
        if (sentence is null) return;

        bool CurrentSelection() => selection == word && session.Document is { } current &&
            current.Id == document.Id && current.Language == document.Language &&
            current.CurrentRevisionId == revision && WordCard.IsVisible;

        await ExplainSelectionAsync(WordExplanationText, CurrentSelection,
            (model, cachedOnly) => session.ExplainWordAsync(model, document.Id, sentence.Id, word.Text,
                cachedOnly: cachedOnly, startUtf16: word.StartUtf16, revisionId: revision));
    }

    private async void DetachedExplainWordClicked(object? sender, EventArgs args)
    {
        if (explainingWord || loadingDetachedSentence || detachedAddress is not { } address ||
            detachedSelection is not { } word || detachedSentence is not { RevisionId: { } revision }) return;

        bool CurrentSelection() => DetachedPdfReader.IsVisible &&
            ReferenceEquals(detachedAddress, address) && ReferenceEquals(detachedSelection, word) &&
            detachedSentence?.RevisionId == revision;

        var session = AppState.Session;
        await ExplainSelectionAsync(DetachedWordExplanationText, CurrentSelection,
            (model, cachedOnly) => session.ExplainDetachedWordAsync(model, address.Document, address.Page, word,
                revisionId: revision, cachedOnly: cachedOnly));
    }

    private async Task ExplainSelectionAsync(Label result, Func<bool> isCurrent,
        Func<string, bool, Task<WordExplanation?>> explain)
    {
        explainingWord = true;
        ExplainWord.IsEnabled = DetachedExplainWord.IsEnabled = false;
        try
        {
            var selected = (await AppState.Session.OptionalModelsAsync())
                .FirstOrDefault(model => model.CanGenerateLanguageHints);
            if (!isCurrent()) return;
            if (selected is not null && await explain(selected.Id, true) is { } saved)
            {
                if (isCurrent()) ShowExplanation(result, selected.Name, saved);
                return;
            }
            if (!isCurrent()) return;
            if (selected is not { FilesPresent: true })
            {
                if (await DisplayAlertAsync("Локальне пояснення",
                    "Для нових пояснень встановіть Qwen 2B у налаштуваннях.",
                    "Налаштування", "Закрити") && isCurrent())
                    await Shell.Current.GoToAsync(AppShell.SettingsRoute);
                return;
            }
            if (!isCurrent()) return;
            result.IsVisible = true;
            result.Text = $"{selected.Name} готує пояснення…";
            var explanation = await explain(selected.Id, false);
            if (isCurrent() && explanation is not null) ShowExplanation(result, selected.Name, explanation);
        }
        catch (Exception error)
        {
            if (isCurrent())
            {
                result.IsVisible = true;
                result.Text = error.Message;
            }
        }
        finally
        {
            explainingWord = false;
            ExplainWord.IsEnabled = true;
            UpdateDetachedExplainButton();
        }
    }

    private void UpdateDetachedExplainButton()
    {
        DetachedExplainWord.IsVisible = !loadingDetachedSentence &&
            detachedSentence?.RevisionId is not null && detachedSelection is not null;
        DetachedExplainWord.IsEnabled = DetachedExplainWord.IsVisible && !explainingWord;
    }

    private static void ShowExplanation(Label result, string model, WordExplanation explanation)
    {
        result.IsVisible = true;
        var detail = explanation.Explanation.Replace(" У цьому реченні — ", $"{Environment.NewLine}Граматична роль: ");
        var label = explanation.Cached ? "Збережена підказка" : "Локальна підказка";
        result.Text = $"{detail}{Environment.NewLine}{label} · {model}";
    }
}
