using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordMeaning[] meanings = [];
    private SenseSnapshot? selectedSense;
    private bool showingMeanings;

    private void SetSelectedSense(SenseSnapshot? sense)
    {
        selectedSense = sense;
        SelectedSenseRow.IsVisible = sense is not null;
        SelectedSense.Text = sense is null ? "" : $"{sense.Lemma} · {sense.PartOfSpeech} — {sense.Gloss}";
    }

    private void ClearSenseClicked(object? sender, EventArgs args) => SetSelectedSense(null);

    private void ShowMeanings(WordLookup? lookup)
    {
        meanings = lookup?.Meanings ?? [];
        OpenMeanings.IsVisible = meanings.Length > 0;
        OpenMeanings.Text = $"Словникові значення · {meanings.Length}";
    }

    private async void OpenMeaningsClicked(object? sender, EventArgs args)
    {
        var selected = meanings;
        if (showingMeanings || selected.Length == 0 || selection is not { } word ||
            AppState.Session.Document is not { } document) return;
        var body = new VerticalStackLayout { Padding = 24, Spacing = 12 };
        body.Children.Add(new Label { Text = word.Text, FontSize = 26, FontAttributes = FontAttributes.Bold });
        var close = new Button { Text = "Закрити", HorizontalOptions = LayoutOptions.Start };
        body.Children.Add(close);
        var page = new ContentPage { Title = "Словникові значення", Content = new ScrollView { Content = body } };
        async Task DismissAsync()
        {
            if (body.IsEnabled && Navigation.ModalStack.LastOrDefault() == page)
            {
                body.IsEnabled = false;
                try { await Navigation.PopModalAsync(); }
                finally { showingMeanings = false; }
            }
        }
        close.Clicked += async (_, _) => await DismissAsync();
        foreach (var meaning in selected)
        {
            var part = new VerticalStackLayout { Spacing = 4 };
            part.Children.Add(new Label { Text = $"{meaning.Lemma} · {meaning.PartOfSpeech}", FontAttributes = FontAttributes.Bold });
            part.Children.Add(new Label { Text = $"{meaning.Gloss} · {DocumentLanguages.Name(meaning.GlossLanguage)}" });
            var match = meaning.ContextMatches > 0
                ? $"Збіги з контекстом · {meaning.ContextMatches}"
                : "Можливе значення";
            part.Children.Add(new Label { Text = match, FontSize = 12 });
            foreach (var example in meaning.Examples)
            {
                var text = example.Translation is { Length: > 0 } ? $"{example.Text}\n{example.Translation}" : example.Text;
                part.Children.Add(new Label { Text = text, FontSize = 14 });
            }
            if (Uri.TryCreate(meaning.SourceUrl, UriKind.Absolute, out var source) &&
                source.Scheme == Uri.UriSchemeHttps && (source.Host == "kaikki.org" || source.Host == "en.wiktionary.org"))
            {
                var link = new Button { Text = $"{meaning.SourceName} · {meaning.SourceLicense}", HorizontalOptions = LayoutOptions.Start };
                link.Clicked += async (_, _) =>
                {
                    try { await Launcher.Default.OpenAsync(source); }
                    catch (Exception error) { await DisplayAlertAsync("Джерело", error.Message, "Гаразд"); }
                };
                part.Children.Add(link);
            }
            else part.Children.Add(new Label { Text = $"{meaning.SourceName} · {meaning.SourceLicense}", FontSize = 12 });
            var pick = new Button
            {
                Text = selectedSense?.Id == meaning.Id ? "Вибране значення" : "Вибрати значення",
                HorizontalOptions = LayoutOptions.Start,
            };
            pick.Clicked += async (_, _) =>
            {
                if (!body.IsEnabled) return;
                if (selection == word && AppState.Session.Document?.Id == document.Id)
                {
                    SetSelectedSense(new SenseSnapshot(meaning.Id, meaning.Lemma, word.Text,
                        meaning.PartOfSpeech, meaning.Gloss, meaning.GlossLanguage,
                        meaning.SourceName, meaning.SourceUrl, meaning.SourceLicense, meaning.SourceRevision));
                    if (VocabularyReading.SelectedIndex > 0 &&
                        vocabularyReadings.ElementAtOrDefault(VocabularyReading.SelectedIndex - 1)?.Expression is not null)
                        VocabularyReading.SelectedIndex = 0;
                }
                await DismissAsync();
            };
            part.Children.Add(pick);
            body.Children.Add(part);
        }
        page.Disappearing += (_, _) => showingMeanings = false;
        showingMeanings = true;
        try { await Navigation.PushModalAsync(page); }
        catch (Exception error)
        {
            showingMeanings = false;
            await DisplayAlertAsync("Словникові значення", error.Message, "Гаразд");
        }
    }
}
