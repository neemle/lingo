using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;
using VocabularyCard = Reader.Contracts.Vocabulary;

namespace Reader.Native.Features.Vocabulary;

public partial class VocabularyPage : ContentPage
{
    private const int MaxArchiveBytes = 256 * 1024 * 1024;

    private sealed record CollectionChoice(string? DocumentId, string Title, string? SourceHash = null);

    private static readonly CollectionChoice AllCards = new(null, "Усі картки");
    private CollectionChoice[] choices = [AllCards];
    private VocabularyCard[] entries = [];
    private bool searching;
    private bool transferring;
    private bool loadingCollection;
    private bool updatingPicker;
    private int appearance;
    private int collectionRevision;
    private int queryRevision;

    private bool sharingCard;
    private bool editingVariants;

    private CollectionChoice ActiveCollection =>
        CollectionPicker.SelectedIndex >= 0 && CollectionPicker.SelectedIndex < choices.Length
            ? choices[CollectionPicker.SelectedIndex]
            : AllCards;

    public VocabularyPage() => InitializeComponent();

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        var session = AppState.Session;
        BindingContext = session;
        var activeAppearance = ++appearance;
        await session.RefreshAsync();
        if (activeAppearance == appearance)
            await RefreshCollectionsAsync(activeAppearance);
    }

    protected override void OnDisappearing()
    {
        appearance++;
        collectionRevision++;
        queryRevision++;
        loadingCollection = false;
        base.OnDisappearing();
    }

    private async Task RefreshCollectionsAsync(int activeAppearance)
    {
        var session = AppState.Session;
        try
        {
            var sources = await session.DictionarySourcesAsync();
            if (activeAppearance != appearance) return;
            var selected = ActiveCollection;
            var documents = session.Snapshot.Documents;
            var byId = documents.ToDictionary(document => document.Id);
            var byHash = documents
                .GroupBy(document => document.SourceHash, StringComparer.Ordinal)
                .ToDictionary(group => group.Key, group => group.First(), StringComparer.Ordinal);

            CollectionChoice ChoiceFor(DictionarySource source)
            {
                if (byId.TryGetValue(source.DocumentId, out var document) ||
                    source.SourceHash is { } hash && byHash.TryGetValue(hash, out document))
                    return new(document.Id, document.Title, document.SourceHash);
                var title = string.IsNullOrWhiteSpace(source.Title)
                    ? $"Документ {ShortId(source.DocumentId)}"
                    : source.Title;
                return new(source.DocumentId, $"{title} (з архіву)", source.SourceHash);
            }

            var namedChoices = sources
                .GroupBy(source => (source.SourceHash is not null, source.SourceHash ?? source.DocumentId))
                .Select(group => ChoiceFor(group.First()))
                .DistinctBy(choice => choice.DocumentId)
                .OrderBy(choice => choice.Title, StringComparer.CurrentCulture)
                .ToArray();
            var duplicateTitles = namedChoices.GroupBy(choice => choice.Title, StringComparer.CurrentCulture)
                .Where(group => group.Count() > 1)
                .Select(group => group.Key)
                .ToHashSet(StringComparer.CurrentCulture);
            choices = [AllCards, .. namedChoices.Select(choice => duplicateTitles.Contains(choice.Title)
                ? choice with { Title = $"{choice.Title} · {ShortId(choice.DocumentId!)}" } : choice)];
            SetPicker(selected);
        }
        catch (Exception error)
        {
            if (activeAppearance != appearance) return;
            ShowArchiveStatus(error.Message);
            choices = [AllCards];
            SetPicker(AllCards);
        }
        await LoadCollectionAsync();
    }

    private void SetPicker(CollectionChoice selected)
    {
        updatingPicker = true;
        try
        {
            CollectionPicker.ItemsSource = choices.Select(choice => choice.Title).ToArray();
            CollectionPicker.SelectedIndex = Math.Max(0, Array.FindIndex(choices, choice =>
                choice.DocumentId == selected.DocumentId ||
                selected.SourceHash is not null && choice.SourceHash == selected.SourceHash));
        }
        finally { updatingPicker = false; }
    }

    private static string ShortId(string id) => id.Length > 8 ? id[..8] : id;

    private async void CollectionChanged(object? sender, EventArgs args)
    {
        if (!updatingPicker) await LoadCollectionAsync();
    }

    private async Task LoadCollectionAsync()
    {
        var activeCollection = ActiveCollection;
        var revision = ++collectionRevision;
        queryRevision++;
        loadingCollection = true;
        try
        {
            if (activeCollection.DocumentId is null)
            {
                entries = AppState.Session.Snapshot.Vocabulary;
            }
            else
            {
                entries = [];
                ShowTextMatches();
                var result = await AppState.Session.DictionaryCollectionAsync(activeCollection.DocumentId);
                if (revision != collectionRevision) return;
                entries = result;
            }
        }
        catch (Exception error)
        {
            if (revision != collectionRevision) return;
            entries = [];
            ShowArchiveStatus(error.Message);
        }
        finally
        {
            if (revision == collectionRevision)
            {
                loadingCollection = false;
                ShowTextMatches();
            }
        }
    }

    private void QueryChanged(object? sender, TextChangedEventArgs args)
    {
        queryRevision++;
        ShowTextMatches();
    }

    private void ShowTextMatches()
    {
        var query = VocabularyQuery.Text?.Trim() ?? "";
        bool Matches(string value) => value.Contains(query, StringComparison.CurrentCultureIgnoreCase);
        VocabularyList.ItemsSource = query.Length == 0 ? entries : entries.Where(entry =>
            Matches(entry.Word) || Matches(entry.Translation) || Matches(entry.Context) ||
            entry.AcceptedTranslations.Any(Matches) || (entry.Sense is { } sense && Matches(sense.Gloss))).ToArray();

        EmptyVocabulary.Text = loadingCollection ? "Завантажуємо добірку…"
            : query.Length > 0 ? "Збігів за текстом немає."
            : ActiveCollection.DocumentId is null ? "Виділіть слово під час читання, щоб додати його сюди."
            : "У цій добірці немає карток.";
        SearchStatus.IsVisible = false;
        CollectionCount.Text = entries.Length.ToString();
        SemanticProperties.SetDescription(CollectionCount, $"Карток у добірці: {entries.Length}");
        UpdateControls();
    }

    private void UpdateControls()
    {
        var ready = !transferring && !loadingCollection;
        CollectionPicker.IsEnabled = !transferring && choices.Length > 1;
        ImportDictionary.IsEnabled = !transferring;
        ExportDictionary.IsEnabled = ready && entries.Length > 0;
        ExportDictionary.Text = ActiveCollection.DocumentId is null ? "Експорт словника" : "Експорт добірки";
        SemanticSearch.IsEnabled = ready && !searching && entries.Length > 0 &&
            !string.IsNullOrWhiteSpace(VocabularyQuery.Text);
    }

    private async void PracticeClicked(object? sender, EventArgs args)
    {
        try { await Shell.Current.GoToAsync("//practice"); }
        catch (Exception error) { ShowArchiveStatus(error.Message); }
    }

    private async void SemanticClicked(object? sender, EventArgs args)
    {
        var query = VocabularyQuery.Text?.Trim() ?? "";
        if (searching || loadingCollection || transferring || query.Length == 0) return;
        var revision = queryRevision;
        var documentId = ActiveCollection.DocumentId;
        searching = true;
        UpdateControls();
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
                    "Налаштування", "Закрити") && revision == queryRevision)
                    await Shell.Current.GoToAsync("//settings");
                return;
            }
            var matches = await session.SemanticVocabularyAsync(query, documentId);
            if (revision != queryRevision) return;
            var byId = entries.ToDictionary(entry => entry.Id);
            var ranked = matches.Where(byId.ContainsKey).Select(id => byId[id]).ToArray();
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
            UpdateControls();
        }
    }

    private async void EditVariantsClicked(object? sender, EventArgs args)
    {
        if (editingVariants || transferring ||
            sender is not Button { CommandParameter: VocabularyCard selected } ||
            entries.FirstOrDefault(entry => entry.Id == selected.Id) is not { } card) return;
        var editor = new Editor
        {
            Text = string.Join(Environment.NewLine, card.AcceptedTranslations),
            Placeholder = "Один варіант на рядок",
            HeightRequest = 150,
        };
        SemanticProperties.SetDescription(editor, "Підтверджені варіанти перекладу");
        var save = new Button { Text = "Зберегти варіанти" };
        var cancel = new Button { Text = "Скасувати" };
        var body = new VerticalStackLayout { Padding = 24, Spacing = 12 };
        body.Children.Add(new Label { Text = $"{card.Word} → {card.Translation}", FontSize = 24 });
        if (card.Sense is { } sense)
        {
            body.Children.Add(new Label { Text = sense.Gloss });
            var attribution = $"{sense.SourceName} · {sense.SourceLicense}";
            if (Uri.TryCreate(sense.SourceUrl, UriKind.Absolute, out var source) &&
                source.Scheme == Uri.UriSchemeHttps && source.Host == "en.wiktionary.org")
            {
                var link = new Button { Text = attribution, HorizontalOptions = LayoutOptions.Start };
                link.Clicked += async (_, _) =>
                {
                    try { await Launcher.Default.OpenAsync(source); }
                    catch (Exception error) { await DisplayAlertAsync("Джерело", error.Message, "Гаразд"); }
                };
                body.Children.Add(link);
            }
            else body.Children.Add(new Label { Text = attribution });
        }
        body.Children.Add(new Label { Text = "Додайте переклади, які підходять саме цій картці. Один варіант на рядок." });
        body.Children.Add(editor);
        body.Children.Add(save);
        body.Children.Add(cancel);
        var page = new ContentPage { Title = "Варіанти перекладу", Content = new ScrollView { Content = body } };
        cancel.Clicked += async (_, _) =>
        {
            if (body.IsEnabled && Navigation.ModalStack.LastOrDefault() == page)
            {
                body.IsEnabled = false;
                await Navigation.PopModalAsync();
            }
        };
        save.Clicked += async (_, _) =>
        {
            if (!body.IsEnabled) return;
            body.IsEnabled = false;
            try
            {
                var variants = (editor.Text ?? "").Split(new[] { '\r', '\n' },
                    StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
                var session = AppState.Session;
                await session.SetVocabularyVariantsAsync(card.Id, variants);
                if (session.Error.Length > 0)
                {
                    await page.DisplayAlertAsync("Варіанти перекладу", session.Error, "Гаразд");
                    return;
                }
                if (Navigation.ModalStack.LastOrDefault() == page)
                    await Navigation.PopModalAsync();
                await RefreshCollectionsAsync(appearance);
            }
            finally { body.IsEnabled = true; }
        };
        page.Disappearing += (_, _) => editingVariants = false;
        editingVariants = true;
        try { await Navigation.PushModalAsync(page); }
        catch (Exception error)
        {
            editingVariants = false;
            ShowArchiveStatus(error.Message);
        }
    }

    private async void ShareCardClicked(object? sender, EventArgs args)
    {
        if (sharingCard || sender is not Button { CommandParameter: VocabularyCard card }) return;
        sharingCard = true;
        try
        {
            var source = DocumentLanguages.Name(card.Language);
            var target = DocumentLanguages.Name(card.TargetLanguage);
            var heading = $"{card.Word} → {card.Translation}";
            var text = $"{heading}{Environment.NewLine}{source} → {target}";
            if (card.AcceptedTranslations.Length > 0)
                text += $"{Environment.NewLine}Варіанти: {card.AcceptedTranslationsLabel}";
            if (card.Sense is { } sense)
                text += $"{Environment.NewLine}{sense.Gloss}{Environment.NewLine}{sense.SourceName} · " +
                    $"{sense.SourceLicense}{Environment.NewLine}{sense.SourceUrl}";
            if (!string.IsNullOrWhiteSpace(card.Context))
                text += $"{Environment.NewLine}{Environment.NewLine}{card.Context}";
            await Share.Default.RequestAsync(new ShareTextRequest(text, "Словникова картка"));
        }
        catch (Exception error) { ShowArchiveStatus(error.Message); }
        finally { sharingCard = false; }
    }

    private async void ExportClicked(object? sender, EventArgs args)
    {
        if (transferring || loadingCollection) return;
        var documentId = ActiveCollection.DocumentId;
        transferring = true;
        UpdateControls();
        ArchiveStatus.IsVisible = false;
        try
        {
            var archive = await AppState.Session.ExportDictionaryAsync(documentId);
            if (archive.Entries.Length == 0)
            {
                ShowArchiveStatus("Немає карток для експорту.");
                return;
            }
            var fileName = documentId is null ? "chytai-dictionary.json" : "chytai-collection.json";
            var bytes = await Task.Run(() => NativeFiles.Encode(archive));
            await NativeFiles.ShareBytesAsync(fileName, bytes);
        }
        catch (Exception error) { ShowArchiveStatus(error.Message); }
        finally
        {
            transferring = false;
            UpdateControls();
        }
    }

    private async void ImportClicked(object? sender, EventArgs args)
    {
        if (transferring) return;
        transferring = true;
        queryRevision++;
        ArchiveStatus.IsVisible = false;
        ShowTextMatches();
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Архів словника Читай JSON" });
            if (file is null) return;
            var bytes = await NativeFiles.ReadBoundedAsync(file, MaxArchiveBytes);
            var archive = await Task.Run(() => NativeFiles.Decode<DictionaryArchive>(bytes));
            if (archive is not { Version: 1, Entries: not null, Reviews: not null, Sources: not null })
                throw new InvalidDataException("Виберіть архів словника Читай.");
            if (archive.Entries.Length == 0)
            {
                ShowArchiveStatus("Архів не містить карток.");
                return;
            }
            if (!await DisplayAlertAsync("Імпорт словника",
                $"Карток в архіві: {archive.Entries.Length}. Повторення буде об'єднано з вашою історією.",
                "Імпортувати", "Скасувати"))
                return;

            var session = AppState.Session;
            await session.ImportDictionaryAsync(archive);
            if (!string.IsNullOrEmpty(session.Error))
                throw new InvalidOperationException(session.Error);
            await RefreshCollectionsAsync(appearance);
            ShowArchiveStatus("Словник імпортовано.");
        }
        catch (Exception error) { ShowArchiveStatus(error.Message); }
        finally
        {
            transferring = false;
            UpdateControls();
        }
    }

    private void ShowArchiveStatus(string text)
    {
        ArchiveStatus.Text = text;
        ArchiveStatus.IsVisible = true;
    }
}
