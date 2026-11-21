using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage : ContentPage
{
    private static readonly HashSet<string> ImageExtensions =
        new([".jpg", ".jpeg", ".png", ".webp", ".heic", ".heif"], StringComparer.OrdinalIgnoreCase);
    private bool importing;

    public LibraryPage() => InitializeComponent();

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        try
        {
            BindingContext = AppState.Session;
            await AppState.Session.RefreshAsync();
            var last = Preferences.Get("lastDocument", "");
            if (!string.IsNullOrEmpty(last)) await AppState.Session.SelectAsync(last);
        }
        catch (Exception error)
        {
            StartupError.Text = $"Не вдалося відкрити локальне ядро: {error.GetBaseException().Message}";
        }
    }

    private async void ImportClicked(object? sender, EventArgs args)
    {
        if (importing || AppState.Session.Busy) return;
        importing = true;
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Додати документ або фотографію" });
            if (file is null) return;
            var bytes = await NativeFiles.ReadBoundedAsync(file, Reader.CoreClient.CoreClient.MaxImportBytes);
            var language = (string?)Language.SelectedItem ?? "en";
            if (ImageExtensions.Contains(Path.GetExtension(file.FileName)))
            {
                ImportStatus.Text = "Розпізнавання тексту на пристрої…";
                var lines = await PhotoOcr.RecognizeAsync(bytes, language);
                if (lines.Length == 0) throw new InvalidDataException("На зображенні не знайдено тексту.");
                await AppState.Session.ImportImageAsync(file.FileName, bytes, language, lines);
            }
            else
            {
                await AppState.Session.ImportAsync(file.FileName, bytes, language);
            }
            if (AppState.Session.Error.Length == 0 && AppState.Session.Document is { } doc)
                await AppState.OpenAsync(doc.Id);
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт", error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            ImportStatus.Text = "";
        }
    }

    private async void DocumentSelected(object? sender, SelectionChangedEventArgs args)
    {
        if (importing || args.CurrentSelection.FirstOrDefault() is not Document document) return;
        ((CollectionView)sender!).SelectedItem = null;
        await AppState.OpenAsync(document.Id);
    }
}
