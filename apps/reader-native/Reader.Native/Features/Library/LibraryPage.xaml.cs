using System.ComponentModel;
using System.Globalization;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Library;

public partial class LibraryPage : ContentPage
{
    private const int MaxDocumentTitleLength = 240;
    private bool importing;
    private bool changingLanguage;
    private bool editingDocument;
    private Task loading = Task.CompletedTask;
    private ReaderSession? observedSession;
    private Snapshot? displayedSnapshot;
    private string displayedFilter = "";
    private bool openingCard;

    public LibraryPage()
    {
        InitializeComponent();
        _ = Task.Run(NativeFiles.PruneExpiredExports);
    }

    protected override void OnAppearing()
    {
        base.OnAppearing();
        loading = LoadLibraryAsync();
    }

    protected override void OnDisappearing()
    {
        if (observedSession is not null)
            observedSession.PropertyChanged -= SessionChanged;
        observedSession = null;
        base.OnDisappearing();
    }

    private void SessionChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(ReaderSession.Snapshot) && sender is ReaderSession session)
            ShowBooks(session.Snapshot);
    }

    private void ShowBooks(Snapshot snapshot)
    {
        var filter = LibraryFilter.Text?.Trim() ?? "";
        if (ReferenceEquals(displayedSnapshot, snapshot) && displayedFilter == filter) return;

        displayedSnapshot = snapshot;
        displayedFilter = filter;
        var cards = snapshot.Documents.Select((document, index) => (document, index))
            .Where(item => filter.Length == 0 || MatchesLibraryFilter(item.document, filter))
            .Select(item => new LibraryCard(item.document, item.index));
        var visible = (filter.Length == 0 ? cards.Append(new LibraryCard(null, 0)) : cards).ToArray();
        Books.ItemsSource = visible;
        NoMatchingBooks.IsVisible = filter.Length != 0 && visible.Length == 0;
    }

    private static bool MatchesLibraryFilter(Document document, string filter)
    {
        var comparison = CultureInfo.InvariantCulture.CompareInfo;
        return string.Equals(document.Language, filter, StringComparison.OrdinalIgnoreCase) ||
            comparison.IndexOf(document.Title, filter, CompareOptions.IgnoreCase) >= 0 ||
            comparison.IndexOf(DocumentLanguages.Name(document.Language), filter, CompareOptions.IgnoreCase) >= 0;
    }

    private void LibraryFilterChanged(object? sender, TextChangedEventArgs args)
    {
        ClearLibraryFilter.IsVisible = !string.IsNullOrEmpty(args.NewTextValue);
        if (displayedSnapshot is { } snapshot) ShowBooks(snapshot);
    }

    private void ClearLibraryFilterClicked(object? sender, EventArgs args) => LibraryFilter.Text = "";

    private void BooksResized(object? sender, EventArgs args)
    {
        if (Books.Width <= 0) return;
        ShelfLayout.Span = Books.Width >= 860 ? 3 : Books.Width >= 560 ? 2 : 1;
        var compact = Books.Width < 710;
        Grid.SetRow(LibraryActions, compact ? 1 : 0);
        Grid.SetColumn(LibraryActions, compact ? 0 : 1);
        Grid.SetColumnSpan(LibraryActions, compact ? 2 : 1);
    }

    private async Task LoadLibraryAsync()
    {
        try
        {
            var session = AppState.Session;
            BindingContext = session;
            if (observedSession is null)
            {
                observedSession = session;
                session.PropertyChanged += SessionChanged;
            }
            ShowBooks(session.Snapshot);
            await session.RefreshAsync();
            await RefreshPendingPdfAsync();
            await RefreshShareInboxAsync();
            var last = Preferences.Get("lastDocument", "");
            if (!string.IsNullOrEmpty(last)) await AppState.Session.SelectAsync(last);
        }
        catch (Exception error)
        {
            StartupError.Text = $"Не вдалося відкрити локальне ядро: {error.GetBaseException().Message}";
        }
    }

    private async void ImportClicked(object? sender, EventArgs args) => await ImportAsync(scanPdf: false);
    private async void ChooseImportSourceClicked(object? sender, EventArgs args)
    {
        if (importing || changingLanguage || !AppState.Session.CanInteract) return;

        const string files = "Вибрати файли або фотографії";
        const string scan = "Розпізнати сканований PDF";
        const string text = "Вставити текст";
        const string image = "Вставити зображення";
        const string article = "Додати статтю за посиланням";

        var choice = await DisplayActionSheetAsync("Додати документ", "Скасувати", null, files, scan, text, image, article);
        switch (choice)
        {
            case files:
                await ImportAsync(scanPdf: false);
                break;
            case scan:
                await ImportAsync(scanPdf: true);
                break;
            case text:
                await PasteTextAsync();
                break;
            case image:
                await PasteImageAsync();
                break;
            case article:
                await ImportArticleAsync(null);
                break;
        }
    }

    private async void FilesDropped(object? sender, DropEventArgs args)
    {
        try
        {
            if (!await DesktopFiles.QueueDropAsync(args))
                ImportStatus.Text = "Серед перетягнутих об’єктів немає підтримуваних файлів, тексту або посилань.";
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт", error.Message, "Гаразд"); }
    }

    private async Task ImportAsync(bool scanPdf)
    {
        if (importing || changingLanguage || AppState.Session.Busy || !DesktopImport.Gate.Wait(0)) return;
        importing = true;
        try
        {
            var options = new PickOptions { PickerTitle = scanPdf ? "Виберіть сканований PDF" : "Додати документи або фотографії" };
            IEnumerable<FileResult?> selected = scanPdf
                ? await FilePicker.Default.PickAsync(options) is { } pdf ? [pdf] : []
                : await FilePicker.Default.PickMultipleAsync(options);
            var staged = new List<ShareInboxItem>();
            var rejected = 0;
            string? firstIssue = null;
            string? capacityIssue = null;
            string? lastDocument = null;
            async Task ProcessBatchAsync()
            {
                ImportStatus.Text = "";
                foreach (var item in staged)
                {
                    try { lastDocument = await ImportInboxItemAsync(item) ?? lastDocument; }
                    catch (Exception error) { await DisplayAlertAsync(item.Name, error.Message, "Гаразд"); }
                }
                staged.Clear();
            }

            foreach (var file in selected.OfType<FileResult>())
            {
                while (true)
                {
                    try
                    {
                        if (scanPdf && !Path.GetExtension(file.FileName).Equals(".pdf", StringComparison.OrdinalIgnoreCase))
                            throw new InvalidDataException("Виберіть файл PDF для розпізнавання.");
                        if (!DesktopFile.Supports(file.FileName))
                            throw new InvalidDataException("Формат файлу не підтримується.");

                        ImportStatus.Text = "Зберігаємо локальні копії для імпорту…";
                        await using var source = await file.OpenReadAsync();
                        staged.Add(await ShareInbox.StageAsync("file", file.FileName, source, scanPdf));
                        break;
                    }
                    catch (ShareInboxFullException) when (staged.Count > 0)
                    {
                        // Committed imports release slots before we retry the same file.
                        await ProcessBatchAsync();
                    }
                    catch (ShareInboxFullException error)
                    {
                        capacityIssue = $"{error.Message} Решту вибраних файлів не додано.";
                        break;
                    }
                    catch (Exception error)
                    {
                        rejected++;
                        firstIssue ??= $"{file.FileName} · {error.Message}";
                        break;
                    }
                }
                if (capacityIssue is not null) break;
            }
            ImportStatus.Text = "";
            if (firstIssue is not null || capacityIssue is not null)
            {
                var failed = firstIssue is null ? "" : $"Не збережено файлів: {rejected}. {firstIssue}";
                await DisplayAlertAsync("Імпорт", $"{failed} {capacityIssue}".Trim(), "Гаразд");
            }

            await ProcessBatchAsync();
            if (lastDocument is not null) await AppState.OpenAsync(lastDocument);
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт", error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            ImportStatus.Text = "";
            DesktopImport.Gate.Release();
            await RefreshShareInboxAsync();
        }
    }

#if !MACCATALYST
    private async Task<string?> ImportLegacyPdfAsync(ShareInboxItem item, byte[] bytes, bool chooseLanguage)
    {
        var session = AppState.Session;
        await session.WaitUntilIdleAsync();
        var language = await ResolveImportLanguageAsync(item, chooseLanguage, () => item.ScanPdf
            ? Task.FromResult<string?>(null) : session.DetectDocumentLanguageAsync(item.Name, bytes));
        if (language is null) return null;

        ImportStatus.Text = "Відкриваємо PDF…";
        var pages = await PdfScan.PageCountAsync(bytes);
        PdfRecognition.CheckPageCount(pages);
        var missing = await session.PdfPagesNeedingOcrAsync(item.Name, bytes, pages);
        if (missing.Length > 0)
        {
            var checkpoint = await Task.Run(() => PdfOcrCheckpoint.Create(item.Name, bytes, language, pages, missing));
            await RemoveShareItemAsync(item);
            ImportStatus.Text = "";
            return await RunPdfOcrAsync(checkpoint, bytes);
        }
        await session.WaitUntilIdleAsync();
        await session.ImportMixedPdfAsync(item.Name, bytes, language, [], pages);
        if (session.Error.Length != 0) throw new InvalidDataException(session.Error);
        return session.Document?.Id;
    }
#endif

    private async Task<string?> ResolveImportLanguageAsync(ShareInboxItem item, bool chooseLanguage,
        Func<Task<string?>> detectLanguage)
    {
        var saved = DocumentLanguages.All.Any(language => language.Code == item.SourceLanguage)
            ? item.SourceLanguage : null;
        if (!chooseLanguage && saved is not null) return saved;
        var suggestion = saved is null ? await detectLanguage() : null;
        var selected = await ChooseLanguageAsync($"Мова оригіналу · {item.Name}", suggestion);
        if (selected is not null && item.SourceLanguage != selected)
            await Task.Run(() => ShareInbox.SaveSourceLanguage(item, selected));
        return selected;
    }

    private async Task<string?> ChooseLanguageAsync(string title, string? suggested = null)
    {
        var choices = DocumentLanguages.All.OrderByDescending(language => language.Code == suggested)
            .Select(language => (Label: language.Code == suggested ? $"{language.Name} · визначено за текстом" : language.Name,
                language.Code)).ToArray();
        var selected = await DisplayActionSheetAsync(title, "Скасувати", null, choices.Select(choice => choice.Label).ToArray());
        return choices.FirstOrDefault(choice => choice.Label == selected).Code;
    }

    private async void ChangeLanguageClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (importing || changingLanguage || session.Busy || sender is not Button { CommandParameter: Document document }
            || !DesktopImport.Gate.Wait(0))
            return;
        changingLanguage = true;
        try
        {
            var language = await ChooseLanguageAsync($"Мова джерела · {document.Title}");
            if (language is not null && language != document.Language)
                await session.SetDocumentLanguageAsync(document.Id, language);
        }
        finally
        {
            changingLanguage = false;
            DesktopImport.Gate.Release();
        }
    }

    private async void RenameDocumentClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (openingCard || session.Busy || sender is not Button { CommandParameter: Document card }
            || !DesktopImport.Gate.Wait(0))
            return;

        editingDocument = true;
        try
        {
            var document = session.Snapshot.Documents.FirstOrDefault(item => item.Id == card.Id);
            if (document is null) return;

            var initial = document.Title.Length <= MaxDocumentTitleLength ? document.Title : "";
            var title = await DisplayPromptAsync("Перейменувати документ", "Введіть нову назву для бібліотеки.",
                "Зберегти", "Скасувати", placeholder: "Назва документа", maxLength: MaxDocumentTitleLength,
                initialValue: initial);
            if (title is not null && title.Trim() != document.Title)
                await session.RenameDocumentAsync(document.Id, title);
        }
        finally
        {
            editingDocument = false;
            DesktopImport.Gate.Release();
        }
    }

    private async void DeleteDocumentClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (openingCard || editingDocument || session.Busy ||
            sender is not Button { CommandParameter: Document card } || !DesktopImport.Gate.Wait(0))
            return;

        editingDocument = true;
        try
        {
            var document = session.Snapshot.Documents.FirstOrDefault(item => item.Id == card.Id);
            if (document is null) return;
            if (!await DisplayAlertAsync("Видалити документ?",
                $"Локальну копію «{document.Title}» та її прогрес буде видалено. Словникові картки й повторення збережуться.",
                "Видалити", "Скасувати")) return;
            await session.DeleteDocumentAsync(document.Id);
            if (session.Error.Length > 0)
            {
                await DisplayAlertAsync("Видалення документа", session.Error, "Гаразд");
                return;
            }
            AppState.ForgetRecentDocument(document.Id);
        }
        finally
        {
            editingDocument = false;
            DesktopImport.Gate.Release();
        }
    }

    private async void DocumentSelected(object? sender, SelectionChangedEventArgs args)
    {
        if (args.CurrentSelection.FirstOrDefault() is not LibraryCard card) return;
        ((CollectionView)sender!).SelectedItem = null;
        await OpenCardAsync(card);
    }

    private async void OpenCardClicked(object? sender, EventArgs args)
    {
        if (sender is Button { CommandParameter: LibraryCard card }) await OpenCardAsync(card);
    }

    private async Task OpenCardAsync(LibraryCard card)
    {
        if (openingCard || importing || changingLanguage || editingDocument || AppState.Session.Busy) return;
        openingCard = true;
        try
        {
            if (card.Document is { } document) await AppState.OpenAsync(document.Id);
            else await ImportAsync(scanPdf: false);
        }
        finally { openingCard = false; }
    }
}

public sealed class LibraryCard
{
    private static readonly (Color Cover, Color Spine, Color Ink)[] Palette =
    [
        (Color.FromArgb("#D9DFCA"), Color.FromArgb("#BCC6A9"), Color.FromArgb("#536141")),
        (Color.FromArgb("#E6D9CC"), Color.FromArgb("#CFBBA8"), Color.FromArgb("#755943")),
        (Color.FromArgb("#CFDADB"), Color.FromArgb("#B1C6C7"), Color.FromArgb("#496669")),
    ];

    public LibraryCard(Document? document, int index)
    {
        Document = document;
        (Cover, Spine, CoverInk) = Palette[index % Palette.Length];
        if (document is { Sentences.Length: > 0 })
            Progress = Math.Clamp((document.Position + 1d) / document.Sentences.Length, 0d, 1d);
    }

    public Document? Document { get; }
    public bool IsDocument => Document is not null;
    public bool IsNew => Document is null;
    public bool HasArticleSource => WebArticles.Address(Document?.SourceUrl) is not null;
    public string Title => Document?.Title ?? "";
    public string LanguageCode => Document?.Language.ToUpperInvariant() ?? "";
    public string Details => Document is { } doc
        ? $"{DocumentLanguages.Name(doc.Language)} · {doc.Sentences.Length} речень" : "";
    public double Progress { get; }
    public string ProgressLabel => $"Позиція читання · {Progress:P0}";
    public Color Cover { get; }
    public Color Spine { get; }
    public Color CoverInk { get; }
}
