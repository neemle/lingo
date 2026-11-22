using System.ComponentModel;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Library;

public partial class LibraryPage : ContentPage
{
    private static readonly HashSet<string> ImageExtensions =
        new([".jpg", ".jpeg", ".png", ".webp", ".heic", ".heif"], StringComparer.OrdinalIgnoreCase);
    private bool importing;
    private bool changingLanguage;
    private Task loading = Task.CompletedTask;
    private ReaderSession? observedSession;
    private Snapshot? displayedSnapshot;
    private bool openingCard;

    public LibraryPage() => InitializeComponent();

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
        if (ReferenceEquals(displayedSnapshot, snapshot)) return;
        displayedSnapshot = snapshot;
        Books.ItemsSource = snapshot.Documents.Select((document, index) => new LibraryCard(document, index))
            .Append(new LibraryCard(null, 0)).ToArray();
    }

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
            var last = Preferences.Get("lastDocument", "");
            if (!string.IsNullOrEmpty(last)) await AppState.Session.SelectAsync(last);
        }
        catch (Exception error)
        {
            StartupError.Text = $"Не вдалося відкрити локальне ядро: {error.GetBaseException().Message}";
        }
    }

    private async void ImportClicked(object? sender, EventArgs args) => await ImportAsync(scanPdf: false);
    private async void ScanPdfClicked(object? sender, EventArgs args) => await ImportAsync(scanPdf: true);

    private async void FilesDropped(object? sender, DropEventArgs args)
    {
        try
        {
            if (await DesktopFiles.QueueDropAsync(args) == 0)
                ImportStatus.Text = "Серед перетягнутих об’єктів немає підтримуваних файлів.";
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт", error.Message, "Гаразд"); }
    }

    private async Task ImportAsync(bool scanPdf)
    {
        if (importing || changingLanguage || AppState.Session.Busy || !DesktopImport.Gate.Wait(0)) return;
        importing = true;
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = scanPdf ? "Виберіть сканований PDF" : "Додати документ або фотографію" });
            if (file is null) return;
            await ImportFileAsync(new DesktopFile(file.FileName,
                () => NativeFiles.ReadBoundedAsync(file, Reader.CoreClient.CoreClient.MaxImportBytes)), scanPdf, external: false);
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт", error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            ImportStatus.Text = "";
            DesktopImport.Gate.Release();
        }
    }

    internal async Task ImportExternalAsync(DesktopFile file)
    {
        await loading;
        importing = true;
        try { await ImportFileAsync(file, scanPdf: false, external: true); }
        catch (Exception error) { await DisplayAlertAsync(file.Name, error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            ImportStatus.Text = "";
        }
    }

    private async Task ImportFileAsync(DesktopFile file, bool scanPdf, bool external)
    {
        var extension = Path.GetExtension(file.Name);
        var isPdf = extension.Equals(".pdf", StringComparison.OrdinalIgnoreCase);
        if (scanPdf && !isPdf) throw new InvalidDataException("Виберіть файл PDF для розпізнавання.");
        var language = await ChooseLanguageAsync(external ? $"Мова оригіналу · {file.Name}" : "Мова оригіналу");
        if (language is null) return;
        var bytes = await file.Read();
        var session = AppState.Session;
        await session.WaitUntilIdleAsync();
        if (scanPdf || (external && isPdf))
        {
            ImportStatus.Text = "Відкриваємо PDF…";
            var pages = await PdfScan.PageCountAsync(bytes);
            if (!scanPdf && pages > PdfRecognition.MaxPages)
                await session.ImportAsync(file.Name, bytes, language);
            else
            {
                PdfRecognition.CheckPageCount(pages);
                var progress = new Progress<int>(page =>
                {
                    if (importing) ImportStatus.Text = $"Розпізнавання сторінки {page}…";
                });
                var missing = await session.PdfPagesNeedingOcrAsync(file.Name, bytes, pages);
                var lines = missing.Length == 0 ? [] : (await PdfScan.RecognizeAsync(bytes, language, missing, progress)).Lines;
                await session.WaitUntilIdleAsync();
                await session.ImportMixedPdfAsync(file.Name, bytes, language, lines, pages);
            }
        }
        else if (ImageExtensions.Contains(extension))
        {
            ImportStatus.Text = "Розпізнавання тексту на пристрої…";
            var lines = await PhotoOcr.RecognizeAsync(bytes, language);
            if (lines.Length == 0) throw new InvalidDataException("На зображенні не знайдено тексту.");
            await session.WaitUntilIdleAsync();
            await session.ImportImageAsync(file.Name, bytes, language, lines);
        }
        else await session.ImportAsync(file.Name, bytes, language);
        if (session.Error.Length == 0 && session.Document is { } doc)
            await AppState.OpenAsync(doc.Id);
    }

    private async Task<string?> ChooseLanguageAsync(string title)
    {
        var selected = await DisplayActionSheetAsync(title, "Скасувати", null,
            DocumentLanguages.All.Select(language => language.Name).ToArray());
        return DocumentLanguages.All.FirstOrDefault(language => language.Name == selected).Code;
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
        if (openingCard || importing || changingLanguage || AppState.Session.Busy) return;
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
