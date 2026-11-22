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

        explainingWord = true;
        ExplainWord.IsEnabled = false;
        try
        {
            var models = (await session.OptionalModelsAsync())
                .Where(model => model.Id is "qwen-0.8b" or "qwen-2b").ToArray();
            if (!CurrentSelection()) return;
            foreach (var model in models)
            {
                var saved = await session.ExplainWordAsync(model.Id, document.Id, sentence.Id, word.Text,
                    cachedOnly: true, startUtf16: word.StartUtf16, revisionId: revision);
                if (!CurrentSelection()) return;
                if (saved is not null)
                {
                    ShowExplanation(model.Name, saved);
                    return;
                }
            }
            var installed = models.Where(model => model.FilesPresent).ToArray();
            if (installed.Length == 0)
            {
                if (await DisplayAlertAsync("Локальне пояснення",
                    "Для пояснень встановіть одну з моделей Qwen у налаштуваннях.",
                    "Налаштування", "Закрити"))
                    await Shell.Current.GoToAsync(AppShell.SettingsRoute);
                return;
            }

            OptionalModel? selected = installed[0];
            if (installed.Length > 1)
            {
                var names = installed.ToDictionary(model => model.Memory?.Ready == false
                    ? $"{model.Name} · бракує пам’яті" : model.Name);
                var choice = await DisplayActionSheetAsync("Модель пояснення", "Скасувати", null, names.Keys.ToArray());
                selected = choice is null ? null : names.GetValueOrDefault(choice);
            }
            if (selected is null || !CurrentSelection()) return;
            WordExplanationText.IsVisible = true;
            WordExplanationText.Text = $"{selected.Name} готує пояснення…";
            var explanation = await session.ExplainWordAsync(selected.Id, document.Id, sentence.Id, word.Text,
                startUtf16: word.StartUtf16, revisionId: revision);
            if (CurrentSelection() && explanation is not null) ShowExplanation(selected.Name, explanation);
        }
        catch (Exception error)
        {
            if (CurrentSelection())
            {
                WordExplanationText.IsVisible = true;
                WordExplanationText.Text = error.Message;
            }
        }
        finally
        {
            explainingWord = false;
            ExplainWord.IsEnabled = true;
        }
    }

    private void ShowExplanation(string model, WordExplanation explanation)
    {
        WordExplanationText.IsVisible = true;
        var label = explanation.Cached ? "Збережене пояснення" : "Припущення";
        WordExplanationText.Text = $"{label} {model} · {explanation.Explanation}";
    }
}
