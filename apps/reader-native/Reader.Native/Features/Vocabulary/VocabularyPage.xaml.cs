using Reader.Native.Services;

namespace Reader.Native.Features.Vocabulary;

public partial class VocabularyPage : ContentPage
{
    private bool searching;
    private int queryRevision;

    public VocabularyPage() => InitializeComponent();

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        BindingContext = AppState.Session;
        await AppState.Session.RefreshAsync();
        ShowTextMatches();
    }

    protected override void OnDisappearing()
    {
        queryRevision++;
        base.OnDisappearing();
    }

    private void QueryChanged(object? sender, TextChangedEventArgs args)
    {
        queryRevision++;
        ShowTextMatches();
    }

    private void ShowTextMatches()
    {
        var query = VocabularyQuery.Text?.Trim() ?? "";
        var entries = AppState.Session.Snapshot.Vocabulary;
        bool Matches(string value) => value.Contains(query, StringComparison.CurrentCultureIgnoreCase);
        VocabularyList.ItemsSource = query.Length == 0 ? entries : entries.Where(entry =>
            Matches(entry.Word) || Matches(entry.Translation) || Matches(entry.Context)).ToArray();
        EmptyVocabulary.Text = query.Length == 0
            ? "Виділіть слово під час читання, щоб додати його сюди." : "Збігів за текстом немає.";
        SemanticSearch.IsEnabled = !searching && query.Length != 0 && entries.Length != 0;
        SearchStatus.IsVisible = false;
    }

    private async void SemanticClicked(object? sender, EventArgs args)
    {
        var query = VocabularyQuery.Text?.Trim() ?? "";
        if (searching || query.Length == 0) return;
        var revision = queryRevision;
        searching = true;
        SemanticSearch.IsEnabled = false;
        SearchStatus.IsVisible = true;
        SearchStatus.Text = "Пошук за змістом…";
        try
        {
            var session = AppState.Session;
            var models = await session.OptionalModelsAsync();
            if (revision != queryRevision) return;
            if (!models.Any(model => model.Id == "multilingual-e5-small" && model.FilesPresent))
            {
                SearchStatus.IsVisible = false;
                if (await DisplayAlertAsync("Семантичний пошук",
                    "Встановіть Multilingual E5 Small у налаштуваннях.",
                    "Налаштування", "Закрити"))
                    await Shell.Current.GoToAsync("//settings");
                return;
            }
            var matches = await session.SemanticVocabularyAsync(query);
            if (revision != queryRevision) return;
            var entries = session.Snapshot.Vocabulary.ToDictionary(entry => entry.Id);
            var ranked = matches.Where(entries.ContainsKey).Select(id => entries[id]).ToArray();
            VocabularyList.ItemsSource = ranked;
            EmptyVocabulary.Text = "Схожих карток немає.";
            SearchStatus.Text = $"Картки за схожістю · {ranked.Length}";
        }
        catch (Exception error)
        {
            if (revision == queryRevision)
            {
                SearchStatus.IsVisible = true;
                SearchStatus.Text = error.Message;
            }
        }
        finally
        {
            searching = false;
            SemanticSearch.IsEnabled = !string.IsNullOrWhiteSpace(VocabularyQuery.Text) &&
                AppState.Session.Snapshot.Vocabulary.Length != 0;
        }
    }
}
