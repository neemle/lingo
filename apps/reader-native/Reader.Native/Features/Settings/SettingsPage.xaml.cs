using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage : ContentPage
{
    private static readonly ModelDownload Downloader = new(new HttpClient(new HttpClientHandler
    {
        UseCookies = false,
    }));
    private TranslationPackageStatus[] downloadChoices = [];
    private CancellationTokenSource? activeDownload;

    public SettingsPage() { InitializeComponent(); BindingContext = AppState.Session; }

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        await RefreshTranslationPackagesAsync();
    }

    private async Task RefreshTranslationPackagesAsync()
    {
        try
        {
            var packages = await AppState.Session.TranslationPackagesAsync();
            TranslationPackages.Text = string.Join(Environment.NewLine, packages.Select(package =>
                $"{LanguageName(package.Source)} → {LanguageName(package.Target)}: " +
                (package.FilesPresent ? "файли на місці" : "пакет відсутній")));
            var previous = ModelChoice.SelectedIndex >= 0 && ModelChoice.SelectedIndex < downloadChoices.Length
                ? downloadChoices[ModelChoice.SelectedIndex].Revision : null;
            downloadChoices = packages.Where(package => !package.FilesPresent).ToArray();
            ModelChoice.ItemsSource = downloadChoices.Select(package =>
                $"{LanguageName(package.Source)} → {LanguageName(package.Target)} " +
                $"({package.Assets.Sum(asset => asset.Bytes) / 1_000_000d:F0} МБ)").ToArray();
            ModelChoice.SelectedIndex = downloadChoices.Length == 0 ? -1
                : Math.Max(0, Array.FindIndex(downloadChoices, package => package.Revision == previous));
            DownloadTranslation.IsEnabled = activeDownload is null && downloadChoices.Length > 0;
        }
        catch (Exception error) { TranslationPackages.Text = error.Message; }
    }

    private static string LanguageName(string code) => code switch
    {
        "en" => "Англійська",
        "fr" => "Французька",
        "uk" => "Українська",
        _ => code,
    };

    private async void ExportClicked(object? sender, EventArgs args)
    {
        try
        {
            var backup = await AppState.Session.ExportAsync();
            await NativeFiles.ShareBytesAsync("reader-backup.json", NativeFiles.Encode(backup));
            Status.Text = "Архів підготовлено. Місце збереження обираєте ви.";
        }
        catch (Exception error) { await DisplayAlertAsync("Резервна копія", error.Message, "Гаразд"); }
    }

    private async void RestoreClicked(object? sender, EventArgs args)
    {
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Архів Читай JSON" });
            if (file is null) return;
            var bytes = await NativeFiles.ReadBoundedAsync(file, 256 * 1024 * 1024);
            var backup = await Task.Run(() => NativeFiles.Decode<Backup>(bytes));
            if (!await DisplayAlertAsync("Відновити архів?",
                "Документи й слова з архіву буде додано до локальної бібліотеки.", "Відновити", "Скасувати")) return;
            await AppState.Session.RestoreAsync(backup);
            Status.Text = AppState.Session.Error.Length == 0 ? "Архів відновлено." : "Відновлення не підтверджено.";
        }
        catch (Exception error) { await DisplayAlertAsync("Відновлення", error.Message, "Гаразд"); }
    }

    private async void OriginalClicked(object? sender, EventArgs args)
    {
        try
        {
            if (AppState.Session.Document is not { } doc) { Status.Text = "Спочатку відкрийте документ."; return; }
            var original = await AppState.Session.OriginalAsync(doc.Id);
            var bytes = original.Bytes.Select(value => checked((byte)value)).ToArray();
            await NativeFiles.ShareBytesAsync(original.Name, bytes);
        }
        catch (Exception error) { await DisplayAlertAsync("Оригінал", error.Message, "Гаразд"); }
    }

    private async void DownloadTranslationClicked(object? sender, EventArgs args)
    {
        var index = ModelChoice.SelectedIndex;
        if (activeDownload is not null || index < 0 || index >= downloadChoices.Length) return;
        var package = downloadChoices[index];
        using var cancellation = new CancellationTokenSource();
        activeDownload = cancellation;

        var root = Path.Combine(FileSystem.CacheDirectory, "translation-downloads");
        var archive = Path.Combine(FileSystem.CacheDirectory, $"translation-{Guid.NewGuid():N}.zip");
        var total = package.Assets.Sum(asset => asset.Bytes);
        var progress = new Progress<long>(bytes =>
        {
            if (activeDownload == cancellation)
                DownloadProgress.Progress = Math.Clamp((double)bytes / total, 0, 1);
        });

        InstallTranslation.IsEnabled = DownloadTranslation.IsEnabled = ModelChoice.IsEnabled = false;
        CancelDownload.IsVisible = DownloadProgress.IsVisible = true;
        DownloadProgress.Progress = 0;
        ModelStatus.Text = "Завантаження пакета…";
        try
        {
            await Task.Run(() => Downloader.FetchArchiveAsync(package, root, archive, progress, cancellation.Token),
                cancellation.Token);
            cancellation.Token.ThrowIfCancellationRequested();
            CancelDownload.IsVisible = false;
            ModelStatus.Text = "Перевірка й встановлення…";
            var pair = await AppState.Session.InstallTranslationPackageAsync(archive);
            ModelStatus.Text = $"Мовний пакет встановлено ({pair}).";
            TryClean(() => ModelDownload.ClearStaging(package, root));
            await RefreshTranslationPackagesAsync();
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            ModelStatus.Text = "Завантаження зупинено. Натисніть «Завантажити пакет», щоб продовжити.";
        }
        catch (Exception error) { ModelStatus.Text = error.Message; }
        finally
        {
            activeDownload = null;
            CancelDownload.IsVisible = DownloadProgress.IsVisible = false;
            InstallTranslation.IsEnabled = ModelChoice.IsEnabled = true;
            DownloadTranslation.IsEnabled = downloadChoices.Length > 0;
            TryClean(() => File.Delete(archive));
        }
    }

    private void CancelDownloadClicked(object? sender, EventArgs args) => activeDownload?.Cancel();

    private void TryClean(Action cleanup)
    {
        try { cleanup(); }
        catch (IOException) { ModelStatus.Text += " Не вдалося видалити тимчасові файли."; }
        catch (UnauthorizedAccessException) { ModelStatus.Text += " Не вдалося видалити тимчасові файли."; }
    }

    private async void InstallTranslationClicked(object? sender, EventArgs args)
    {
        InstallTranslation.IsEnabled = DownloadTranslation.IsEnabled = false;
        var path = Path.Combine(FileSystem.CacheDirectory, $"translation-{Guid.NewGuid():N}.zip");
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Мовний пакет ZIP" });
            if (file is null) return;
            Status.Text = "Перевірка мовного пакета…";
            await using (var input = await file.OpenReadAsync())
            await using (var output = File.Create(path))
            {
                var buffer = new byte[64 * 1024];
                int count;
                while ((count = await input.ReadAsync(buffer)) != 0)
                {
                    if (output.Length + count > 400_000_000)
                        throw new InvalidDataException("Архів мовного пакета завеликий.");
                    await output.WriteAsync(buffer.AsMemory(0, count));
                }
            }
            var pair = await AppState.Session.InstallTranslationPackageAsync(path);
            Status.Text = $"Мовний пакет встановлено ({pair}).";
            await RefreshTranslationPackagesAsync();
        }
        catch (Exception error) { Status.Text = error.Message; }
        finally
        {
            try { File.Delete(path); }
            catch (IOException) { Status.Text += " Не вдалося видалити тимчасову копію архіву."; }
            catch (UnauthorizedAccessException) { Status.Text += " Не вдалося видалити тимчасову копію архіву."; }
            finally
            {
                InstallTranslation.IsEnabled = true;
                DownloadTranslation.IsEnabled = downloadChoices.Length > 0;
            }
        }
    }

    private async void ProbeClicked(object? sender, EventArgs args) =>
        await Navigation.PushAsync(new ProbePage());
}
