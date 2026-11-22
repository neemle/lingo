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

        await ExplainSelectionAsync(WordExplanationText, WordExplanationBusy, document.Id, CurrentSelection,
            (model, cachedOnly) => session.ExplainWordAsync(model, document.Id, sentence.Id, word.Text,
                cachedOnly: cachedOnly, startUtf16: word.StartUtf16, revisionId: revision));
    }

    private async void DetachedExplainWordClicked(object? sender, EventArgs args)
    {
        if (explainingWord || loadingDetachedSentence || detachedAddress is not { } address ||
            detachedSelection is not { } word || detachedSentence is not { RevisionId: { } revision }) return;

        bool CurrentSelection() => DetachedPdfReader.IsVisible && DetachedWordCard.IsVisible &&
            ReferenceEquals(detachedAddress, address) && ReferenceEquals(detachedSelection, word) &&
            detachedSentence?.RevisionId == revision;

        var session = AppState.Session;
        await ExplainSelectionAsync(DetachedWordExplanationText, DetachedWordExplanationBusy, address.Document, CurrentSelection,
            (model, cachedOnly) => session.ExplainDetachedWordAsync(model, address.Document, address.Page, word,
                revisionId: revision, cachedOnly: cachedOnly));
    }

    private async Task ExplainSelectionAsync(Label result, ActivityIndicator spinner, string documentId,
        Func<bool> isCurrent, Func<string, bool, Task<WordExplanation?>> explain)
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
                await ShowExplanationAsync(result, spinner, documentId, selected.Name, saved, isCurrent);
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
            spinner.IsVisible = spinner.IsRunning = true;
            result.IsVisible = true;
            result.Text = $"{selected.Name} готує пояснення…";
            var explanation = await explain(selected.Id, false);
            if (explanation is not null)
                await ShowExplanationAsync(result, spinner, documentId, selected.Name, explanation, isCurrent);
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
            spinner.IsRunning = spinner.IsVisible = false;
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

    private static async Task ShowExplanationAsync(Label result, ActivityIndicator spinner, string documentId,
        string model, WordExplanation explanation, Func<bool> isCurrent)
    {
        if (!isCurrent()) return;
        var detail = explanation.Explanation.Replace(" У цьому реченні — ", $"{Environment.NewLine}Граматична роль: ");
        var label = explanation.Cached ? "Збережена підказка" : "Локальна підказка";
        if (explanation.Gloss is { } gloss)
        {
            string? ukrainian = gloss.Language == "uk" ? gloss.Text : null;
            if (ukrainian is null)
            {
                spinner.IsVisible = spinner.IsRunning = true;
                result.IsVisible = true;
                result.Text = "Перекладаємо словникове визначення…";
                try
                {
                    ukrainian = (await AppState.Session.TranslateDictionaryGlossAsync(documentId, gloss.Language, gloss.Text))?.Text;
                }
                catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
            }
            if (!isCurrent()) return;
            detail += $"{Environment.NewLine}{ukrainian ?? gloss.Text}";
            label = ukrainian is null ? $"{label} · Wiktionary" : $"{label} · Wiktionary, офлайн-переклад";
        }
        result.IsVisible = true;
        result.Text = $"{detail}{Environment.NewLine}{label} · {model}";
    }
}
