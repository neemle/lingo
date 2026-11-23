using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordMeaning[] meanings = [];
    private sealed record ChosenWordMeaning(SenseSnapshot? Sense, string Translation, string Target);
    private ChosenWordMeaning? canonicalSense;
    private ChosenWordMeaning? detachedSense;
    private SenseSnapshot? selectedSense => canonicalSense?.Sense;

    private void SetSelectedSense(ChosenWordMeaning? meaning)
    {
        canonicalSense = meaning;
        SelectedSense.Text = meaning?.Translation;
        SelectedSenseRow.IsVisible = meaning is not null;
        if (WordCard.IsVisible) UpdatePopupGloss(detached: false);
    }

    private void SetDetachedSense(ChosenWordMeaning? meaning)
    {
        detachedSense = meaning;
        DetachedClearSense.IsVisible = meaning is not null;
        if (DetachedWordCard.IsVisible) UpdatePopupGloss(detached: true);
    }

    private void ChooseCanonicalMeaning(ChosenWordMeaning meaning)
    {
        SetSelectedSense(meaning);
        Translation.Text = meaning.Translation;
        if (VocabularyReading.SelectedIndex > 0 &&
            vocabularyReadings.ElementAtOrDefault(VocabularyReading.SelectedIndex - 1)?.Expression is not null)
            VocabularyReading.SelectedIndex = 0;
    }

    private void ClearSenseClicked(object? sender, EventArgs args)
    {
        if (DetachedWordCard.IsVisible)
        {
            SetDetachedSense(null);
            if (PopupMachineGloss(detached: true) is null)
                _ = LoadPopupTranslationAsync(detached: true);
            return;
        }
        var previous = canonicalSense;
        SetSelectedSense(null);
        if (previous is not null && Translation.Text == previous.Translation)
            Translation.Text = PopupMachineGloss(detached: false);
        if (PopupMachineGloss(detached: false) is null)
            _ = LoadPopupTranslationAsync(detached: false);
    }

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
        SetDetachedSense(null);
        DetachedMeaningsButton.Text = "Значення";
        DetachedMeanings.Children.Clear();
        DetachedMeanings.IsVisible = false;
    }

    private async void OpenMeaningsClicked(object? sender, EventArgs args)
    {
        if (selection is not { } word || AppState.Session.Document is not { } document ||
            TargetCode is not { } target || meanings.Length == 0) return;
        WordMeanings.IsVisible = !WordMeanings.IsVisible;
        OpenMeanings.Text = WordMeanings.IsVisible ? "Згорнути значення" : $"Значення · {meanings.Length}";
        PositionWordPopup();
        if (WordMeanings.IsVisible && WordMeanings.Children.Count == 0)
            await ShowMeaningRowsAsync(WordMeanings, word, document.Id, document.Language, target, meanings,
                (sense, text) => ChooseCanonicalMeaning(new(sense, text, target)));
    }

    private async void OpenDetachedMeaningsClicked(object? sender, EventArgs args)
    {
        if (detachedSelection is not { } word || detachedAddress is not { } address ||
            TargetCode is not { } target) return;
        var revision = detachedSentence?.RevisionId;
        DetachedMeanings.IsVisible = !DetachedMeanings.IsVisible;
        DetachedMeaningsButton.Text = DetachedMeanings.IsVisible ? "Згорнути значення" : "Значення";
        PositionWordPopup();
        if (!DetachedMeanings.IsVisible || DetachedMeanings.Children.Count > 0) return;

        var status = new Label { Text = "Пошук значень…" };
        DetachedMeanings.Children.Add(status);
        try
        {
            var lookup = await LookupDetachedCardAsync(address, revision, word);
            if (!DetachedMeanings.Children.Contains(status) || !ReferenceEquals(detachedAddress, address) ||
                detachedSentence?.RevisionId != revision || detachedSelection != word) return;
            DetachedMeanings.Children.Clear();
            SetDetachedSyntaxHint(lookup?.SyntaxHint);
            await ShowMeaningRowsAsync(DetachedMeanings, word, address.Document, address.Language, target,
                lookup?.Meanings ?? [], (sense, text) => SetDetachedSense(new(sense, text, target)));
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

        var translations = new Dictionary<(string Text, string Language), string?>();
        string? Localized(string text, string language) =>
            language == target ? text : translations.GetValueOrDefault((text, language));

        async Task LocalizeAsync(IEnumerable<(string Text, string Language)> inputs)
        {
            var groups = inputs.Distinct().Where(item => item.Language != target &&
                !translations.ContainsKey(item) && !string.IsNullOrWhiteSpace(item.Text) &&
                System.Text.Encoding.UTF8.GetByteCount(item.Text) <= 4096).GroupBy(item => item.Language);
            foreach (var group in groups)
            {
                foreach (var batch in group.Select(item => item.Text).Chunk(12))
                {
                    if (!Current()) return;
                    try
                    {
                        var output = await AppState.Session.TranslateDictionaryGlossesAsync(document, group.Key, target, batch);
                        for (var index = 0; index < batch.Length; index++)
                            translations[(batch[index], group.Key)] = output[index];
                    }
                    catch (Exception error)
                    {
                        System.Diagnostics.Debug.WriteLine(error.Message);
                        if (batch.Length == 1) continue;
                        foreach (var text in batch)
                        {
                            if (!Current()) return;
                            try
                            {
                                translations[(text, group.Key)] = (await AppState.Session
                                    .TranslateDictionaryGlossAsync(document, group.Key, text, target))?.Text;
                            }
                            catch (Exception entryError) { System.Diagnostics.Debug.WriteLine(entryError.Message); }
                        }
                    }
                }
            }
        }

        void ShowGloss(WordMeaning meaning, Label label, VerticalStackLayout detail)
        {
            var gloss = Localized(meaning.Gloss, meaning.GlossLanguage);
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

        foreach (var (meaning, label, detail, _) in rows.Where(row => row.Meaning.GlossLanguage == target))
            ShowGloss(meaning, label, detail);
        await LocalizeAsync(ordered.Select(meaning => (meaning.Gloss, meaning.GlossLanguage)));
        if (!Current()) return;
        foreach (var (meaning, label, detail, _) in rows.Where(row => row.Meaning.GlossLanguage != target))
            ShowGloss(meaning, label, detail);

        var examples = new List<(MeaningExample Example, string GlossLanguage, VerticalStackLayout Host)>();
        foreach (var (meaning, _, detail, _) in rows)
        {
            foreach (var example in meaning.Examples)
            {
                var exampleHost = new VerticalStackLayout { Spacing = 4 };
                exampleHost.Children.Add(WordDetailAudioRow(example.Text, source, 15));
                detail.Children.Add(exampleHost);
                if (meaning.GlossLanguage == target && example.Translation is { Length: > 0 } localized)
                    exampleHost.Children.Add(WordDetailAudioRow(localized, target, 14));
                else examples.Add((example, meaning.GlossLanguage, exampleHost));
            }
        }
        await LocalizeAsync(examples.Select(row => (row.Example.Text, source)));
        if (!Current()) return;
        var remaining = examples.Where(row => string.IsNullOrWhiteSpace(Localized(row.Example.Text, source)) &&
            row.Example.Translation is { Length: > 0 }).ToArray();
        foreach (var (example, _, exampleHost) in examples)
            if (Localized(example.Text, source) is { Length: > 0 } translation)
                exampleHost.Children.Add(WordDetailAudioRow(translation, target, 14));
        await LocalizeAsync(remaining.Select(row => (row.Example.Translation!, row.GlossLanguage)));
        if (!Current()) return;
        foreach (var (example, language, exampleHost) in remaining)
            if (Localized(example.Translation!, language) is { Length: > 0 } translation)
                exampleHost.Children.Add(WordDetailAudioRow(translation, target, 14));
    }

    private static SenseSnapshot MeaningSnapshot(string surface, WordMeaning meaning) =>
        new(meaning.Id, meaning.Lemma, surface, meaning.PartOfSpeech, meaning.Gloss, meaning.GlossLanguage,
            meaning.SourceName, meaning.SourceUrl, meaning.SourceLicense, meaning.SourceRevision);
}
