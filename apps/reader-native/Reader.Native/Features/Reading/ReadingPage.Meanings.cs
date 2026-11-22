using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordMeaning[] meanings = [];
    private SenseSnapshot? selectedSense;
    private (SenseSnapshot Sense, string Translation)? detachedSense;

    private void SetSelectedSense(SenseSnapshot? sense)
    {
        selectedSense = sense;
        SelectedSenseRow.IsVisible = sense is not null;
    }

    private void ClearSenseClicked(object? sender, EventArgs args) => SetSelectedSense(null);

    private void ShowMeanings(WordLookup? lookup)
    {
        meanings = lookup?.Meanings ?? [];
        WordMeanings.Children.Clear();
        WordMeanings.IsVisible = false;
        OpenMeanings.IsVisible = meanings.Length > 0;
        OpenMeanings.Text = $"Значення · {meanings.Length}";
    }

    private void ResetDetachedMeanings()
    {
        detachedSense = null;
        DetachedMeaningsButton.Text = "Значення";
        DetachedMeanings.Children.Clear();
        DetachedMeanings.IsVisible = false;
        DetachedSyntaxHint.IsVisible = false;
    }

    private async void OpenMeaningsClicked(object? sender, EventArgs args)
    {
        if (selection is not { } word || AppState.Session.Document is not { } document ||
            TargetCode is not { } target || meanings.Length == 0) return;
        WordMeanings.IsVisible = !WordMeanings.IsVisible;
        OpenMeanings.Text = WordMeanings.IsVisible ? "Згорнути значення" : $"Значення · {meanings.Length}";
        PositionWordPopup();
        if (WordMeanings.IsVisible && WordMeanings.Children.Count == 0)
            await ShowMeaningRowsAsync(WordMeanings, word, document.Id, document.Language, target, meanings, (sense, text) =>
            {
                SetSelectedSense(sense);
                SelectedSense.Text = text;
                Translation.Text = text;
                if (VocabularyReading.SelectedIndex > 0 &&
                    vocabularyReadings.ElementAtOrDefault(VocabularyReading.SelectedIndex - 1)?.Expression is not null)
                    VocabularyReading.SelectedIndex = 0;
            });
    }

    private async void OpenDetachedMeaningsClicked(object? sender, EventArgs args)
    {
        if (detachedSelection is not { } word || detachedAddress is not { } address ||
            TargetCode is not { } target) return;
        DetachedMeanings.IsVisible = !DetachedMeanings.IsVisible;
        DetachedMeaningsButton.Text = DetachedMeanings.IsVisible ? "Згорнути значення" : "Значення";
        PositionWordPopup();
        if (!DetachedMeanings.IsVisible || DetachedMeanings.Children.Count > 0) return;

        var status = new Label { Text = "Пошук значень…" };
        DetachedMeanings.Children.Add(status);
        try
        {
            var lookup = await AppState.Session.LookupDetachedPdfWordAsync(address.Document, address.Page, word);
            if (!DetachedMeanings.Children.Contains(status)) return;
            DetachedMeanings.Children.Clear();
            DetachedSyntaxHint.Text = lookup?.SyntaxHint;
            DetachedSyntaxHint.IsVisible = lookup?.SyntaxHint is not null;
            await ShowMeaningRowsAsync(DetachedMeanings, word, address.Document, address.Language, target,
                lookup?.Meanings ?? [], (sense, text) =>
                {
                    detachedSense = (sense, text);
                    DetachedWordTranslationGloss.Text = text;
                    DetachedSpeakWordTranslation.IsEnabled = true;
                });
        }
        catch (Exception error) { status.Text = error.Message; }
    }

    private async Task ShowMeaningRowsAsync(VerticalStackLayout host, TextSelection word, string document,
        string source, string target, WordMeaning[] entries, Action<SenseSnapshot, string> choose)
    {
        if (entries.Length == 0)
        {
            host.Children.Add(new Label { Text = "Для цього виділення немає словникових значень." });
            return;
        }

        var ordered = entries.OrderByDescending(meaning => meaning.GlossLanguage == target).ToArray();
        var rows = ordered.Select(meaning =>
        {
            var label = new Label { Text = "Перекладаємо значення…", FontSize = 13 };
            var detail = new VerticalStackLayout { Spacing = 5, Children = { label } };
            var row = new Border
            {
                Padding = new Thickness(10, 7), Stroke = Color.FromArgb("#E1DFD4"),
                BackgroundColor = Color.FromArgb("#F7F6EF"), Content = detail,
            };
            host.Children.Add(row);
            return (Meaning: meaning, Label: label, Detail: detail, Row: row);
        }).ToArray();
        foreach (var entry in ordered.DistinctBy(meaning => (meaning.SourceName, meaning.SourceLicense)))
            host.Children.Add(new Label { Text = $"{entry.SourceName} · {entry.SourceLicense}", FontSize = 11 });

        bool Current() => host.Children.Contains(rows[0].Row) && TargetCode == target &&
            AppState.Session.Document?.Id == document &&
            (host == WordMeanings ? WordCard.IsVisible && selection == word
                : DetachedWordCard.IsVisible && detachedSelection == word);

        async Task<string?> LocalizeAsync(string text, string language)
        {
            if (language == target) return text;
            try
            {
                return (await AppState.Session.TranslateDictionaryGlossAsync(document, language, text, target))?.Text;
            }
            catch (Exception error)
            {
                System.Diagnostics.Debug.WriteLine(error.Message);
                return null;
            }
        }

        foreach (var (meaning, label, detail, _) in rows)
        {
            if (!Current()) return;
            var gloss = await LocalizeAsync(meaning.Gloss, meaning.GlossLanguage);
            if (!Current()) return;
            if (string.IsNullOrWhiteSpace(gloss))
            {
                label.Text = $"Значення недоступне мовою перекладу ({DocumentLanguages.Name(target)}).";
            }
            else
            {
                detail.Children.Remove(label);
                detail.Children.Add(WordDetailAudioRow(gloss, target, 17));
                var pick = new Button
                {
                    Text = "Використати значення", HorizontalOptions = LayoutOptions.Start,
                    Style = (Style)Resources["PlayerButton"],
                };
                pick.Clicked += (_, _) =>
                {
                    if (Current()) choose(MeaningSnapshot(word.Text, meaning), gloss);
                };
                detail.Children.Add(pick);
            }
        }

        foreach (var (meaning, _, detail, _) in rows)
        {
            if (!Current()) return;
            foreach (var example in meaning.Examples)
            {
                detail.Children.Add(WordDetailAudioRow(example.Text, source, 15));
                var translation = example.Translation is { Length: > 0 } value
                    ? await LocalizeAsync(value, meaning.GlossLanguage)
                    : await LocalizeAsync(example.Text, source);
                if (!Current()) return;
                if (!string.IsNullOrWhiteSpace(translation))
                    detail.Children.Add(WordDetailAudioRow(translation, target, 14));
            }
        }
    }

    private static SenseSnapshot MeaningSnapshot(string surface, WordMeaning meaning) =>
        new(meaning.Id, meaning.Lemma, surface, meaning.PartOfSpeech, meaning.Gloss, meaning.GlossLanguage,
            meaning.SourceName, meaning.SourceUrl, meaning.SourceLicense, meaning.SourceRevision);
}
