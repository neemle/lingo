using Reader.Contracts;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordMeaning[] meanings = [];

    private void ShowMeanings(WordLookup? lookup)
    {
        meanings = lookup?.Meanings ?? [];
        OpenMeanings.IsVisible = meanings.Length > 0;
        OpenMeanings.Text = $"Словникові значення · {meanings.Length}";
    }

    private async void OpenMeaningsClicked(object? sender, EventArgs args)
    {
        var selected = meanings;
        if (selected.Length == 0 || selection is not { } word) return;
        var body = new VerticalStackLayout { Padding = 24, Spacing = 12 };
        body.Children.Add(new Label { Text = word.Text, FontSize = 26, FontAttributes = FontAttributes.Bold });
        var close = new Button { Text = "Закрити", HorizontalOptions = LayoutOptions.Start };
        body.Children.Add(close);
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
            body.Children.Add(part);
        }
        var page = new ContentPage { Title = "Словникові значення", Content = new ScrollView { Content = body } };
        close.Clicked += async (_, _) => await Navigation.PopModalAsync();
        await Navigation.PushModalAsync(page);
    }
}
