using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private CancellationTokenSource? wordExplanationRun;

    private async void ExplainWordClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (wordExplanationRun is not null || selection is not { } word ||
            session.Document is not { CurrentRevisionId: { } revision } document) return;
        var sentence = Array.Find(document.Sentences,
            item => word.StartUtf16 >= item.StartUtf16 && word.StartUtf16 < item.EndUtf16);
        if (sentence is null) return;

        var target = TargetCode;
        bool CurrentSelection() => TargetCode == target && selection == word && session.Document is { } current &&
            current.Id == document.Id && current.Language == document.Language &&
            current.CurrentRevisionId == revision && WordCard.IsVisible;

        await ExplainSelectionAsync(WordExplanationText, WordExplanationGloss, WordExplanationBusy,
            document.Id, target ?? "uk", CurrentSelection,
            (model, cachedOnly, token) => session.ExplainWordAsync(model, document.Id, sentence.Id, word.Text,
                cachedOnly: cachedOnly, startUtf16: word.StartUtf16, revisionId: revision, cancellationToken: token));
    }

    private async void DetachedExplainWordClicked(object? sender, EventArgs args)
    {
        if (wordExplanationRun is not null || loadingDetachedSentence || detachedAddress is not { } address ||
            detachedSelection is not { } word || detachedSentence is not { RevisionId: { } revision }) return;

        var target = TargetCode;
        bool CurrentSelection() => TargetCode == target && DetachedPdfReader.IsVisible && DetachedWordCard.IsVisible &&
            ReferenceEquals(detachedAddress, address) && ReferenceEquals(detachedSelection, word) &&
            detachedSentence?.RevisionId == revision;

        var session = AppState.Session;
        await ExplainSelectionAsync(DetachedWordExplanationText, DetachedWordExplanationGloss, DetachedWordExplanationBusy,
            address.Document, target ?? "uk", CurrentSelection,
            (model, cachedOnly, token) => session.ExplainDetachedWordAsync(model, address.Document, address.Page, word,
                revisionId: revision, cachedOnly: cachedOnly, cancellationToken: token));
    }

    private static void ResetExplanation(Label result, VerticalStackLayout glossHost)
    {
        result.Text = "";
        result.IsVisible = false;
        glossHost.Children.Clear();
        glossHost.IsVisible = false;
    }

    private async Task ExplainSelectionAsync(Label result, VerticalStackLayout glossHost, ActivityIndicator spinner,
        string documentId, string target, Func<bool> isCurrent, Func<string, bool, CancellationToken, Task<WordExplanation?>> explain)
    {
        ResetExplanation(result, glossHost);
        using var run = new CancellationTokenSource();
        wordExplanationRun = run;
        var token = run.Token;
        bool Current() => ReferenceEquals(wordExplanationRun, run) && !token.IsCancellationRequested && isCurrent();
        void Apply(string text)
        {
            if (!isCurrent()) return;
            var meaning = new ChosenWordMeaning(null, text, target);
            var detached = ReferenceEquals(glossHost, DetachedWordExplanationGloss);
            if (detached) SetDetachedSense(meaning);
            else ChooseCanonicalMeaning(meaning);
            _ = (detached ? DetachedWordCardScroll : WordCardScroll).ScrollToAsync(0, 0, animated: true);
        }
        ExplainWord.IsEnabled = DetachedExplainWord.IsEnabled = false;
        try
        {
            var selected = (await AppState.Session.OptionalModelsAsync())
                .FirstOrDefault(model => model.CanGenerateLanguageHints);
            if (!Current()) return;
            if (selected is not null && await explain(selected.Id, true, token) is { } saved)
            {
                await ShowExplanationAsync(result, glossHost, spinner, documentId, target, selected.Name, saved, Current, Apply);
                return;
            }
            if (!Current()) return;
            if (selected is not { FilesPresent: true })
            {
                if (await DisplayAlertAsync("Локальне пояснення",
                    "Для нових пояснень встановіть Qwen 2B у налаштуваннях.",
                    "Налаштування", "Закрити") && Current())
                    await Shell.Current.GoToAsync(AppShell.SettingsRoute);
                return;
            }
            if (!Current()) return;
            spinner.IsVisible = spinner.IsRunning = true;
            result.IsVisible = true;
            result.Text = $"{selected.Name} готує пояснення…";
            var explanation = await explain(selected.Id, false, token);
            if (explanation is not null)
                await ShowExplanationAsync(result, glossHost, spinner, documentId, target, selected.Name, explanation, Current, Apply);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception error)
        {
            if (Current())
            {
                result.IsVisible = true;
                result.Text = error.Message;
            }
        }
        finally
        {
            if (ReferenceEquals(wordExplanationRun, run)) FinishWordExplanation();
        }
    }

    private void CancelWordExplanation()
    {
        if (wordExplanationRun is not { } run) return;
        _ = run.CancelAsync();
        FinishWordExplanation();
    }

    private void FinishWordExplanation()
    {
        wordExplanationRun = null;
        WordExplanationBusy.IsRunning = WordExplanationBusy.IsVisible = false;
        DetachedWordExplanationBusy.IsRunning = DetachedWordExplanationBusy.IsVisible = false;
        ExplainWord.IsEnabled = true;
        UpdateDetachedExplainButton();
    }

    private void UpdateDetachedExplainButton()
    {
        DetachedExplainWord.IsVisible = !loadingDetachedSentence &&
            detachedSentence?.RevisionId is not null && detachedSelection is not null;
        DetachedExplainWord.IsEnabled = DetachedExplainWord.IsVisible && wordExplanationRun is null;
    }

    private async Task ShowExplanationAsync(Label result, VerticalStackLayout glossHost, ActivityIndicator spinner,
        string documentId, string target, string model, WordExplanation explanation, Func<bool> isCurrent, Action<string> apply)
    {
        if (!isCurrent()) return;
        var detail = explanation.Explanation.Replace(" У цьому реченні — ", $"{Environment.NewLine}Граматична роль: ");
        var label = explanation.Cached ? "Збережена підказка" : "Локальна підказка";
        var proposed = explanation.Gloss ?? (explanation.ModelGloss is { } modelGloss ? new DictionaryGloss(modelGloss, "uk") : null);
        if (proposed is { } gloss)
        {
            string? localized = gloss.Language == target ? gloss.Text : null;
            if (localized is null)
            {
                spinner.IsVisible = spinner.IsRunning = true;
                result.IsVisible = true;
                result.Text = "Перекладаємо словникове визначення…";
                try
                {
                    localized = (await AppState.Session.TranslateDictionaryGlossAsync(documentId, gloss.Language,
                        gloss.Text, target))?.Text;
                }
                catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
            }
            if (!isCurrent()) return;
            if (string.IsNullOrWhiteSpace(localized))
                glossHost.Children.Add(new Label { Text = $"Значення недоступне мовою перекладу ({DocumentLanguages.Name(target)})." });
            else
            {
                glossHost.Children.Add(WordDetailAudioRow(localized, target, 15));
                var use = new Button
                {
                    Text = "Використати переклад", Style = (Style)Resources["PlayerButton"],
                    HorizontalOptions = LayoutOptions.Start,
                };
                DescribeButton(use, "Підставити цей переклад у картку слова");
                use.Clicked += (_, _) =>
                {
                    if (glossHost.Children.Contains(use)) apply(localized);
                };
                glossHost.Children.Add(use);
            }
            glossHost.IsVisible = true;
            if (explanation.Gloss is not null) label += " · Wiktionary";
            if (gloss.Language != target && !string.IsNullOrWhiteSpace(localized)) label += " · офлайн-переклад";
        }
        result.IsVisible = true;
        result.Text = $"{detail}{Environment.NewLine}{label} · {model}";
    }
}
