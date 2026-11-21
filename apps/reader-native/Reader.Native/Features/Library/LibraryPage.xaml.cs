using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage : ContentPage
{
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
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Додати TXT, DOCX або PDF" });
            if (file is null) return;
            var bytes = await NativeFiles.ReadBoundedAsync(file, Reader.CoreClient.CoreClient.MaxImportBytes);
            await AppState.Session.ImportAsync(file.FileName, bytes, (string?)Language.SelectedItem ?? "en");
            if (AppState.Session.Error.Length == 0 && AppState.Session.Document is { } doc)
                await AppState.OpenAsync(doc.Id);
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт", error.Message, "Гаразд"); }
    }

    private async void DocumentSelected(object? sender, SelectionChangedEventArgs args)
    {
        if (args.CurrentSelection.FirstOrDefault() is not Document document) return;
        ((CollectionView)sender!).SelectedItem = null;
        await AppState.OpenAsync(document.Id);
    }
}
