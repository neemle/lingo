using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private bool loadingStorage;
    private bool changingStorage;

    private static readonly string[] TemporaryDownloads =
        ["model-transport", "translation-downloads", "knowledge-downloads", "lexical-downloads", "optional-model-downloads"];
    private static readonly string[] LocalIntake = ["share-inbox", "pdf-ocr"];

    private async void RefreshStorageClicked(object? sender, EventArgs args) => await RefreshStorageAsync();

    private async Task RefreshStorageAsync()
    {
        loadingStorage = true;
        StorageControls.IsEnabled = false;
        try
        {
            var session = AppState.Session;
            BackgroundTranslationSwitch.IsToggled = session.GetReaderPreferences().BackgroundTranslation == true;
            var usage = await session.StorageUsageAsync();
            var files = await Task.Run(() =>
            {
                var downloads = TemporaryDownloads.Sum(name => FolderSize(Path.Combine(FileSystem.CacheDirectory, name)));
                var intake = LocalIntake.Sum(name => FolderSize(Path.Combine(FileSystem.AppDataDirectory, name)));
                return (downloads, intake);
            });
            var cache = usage.DatabaseCacheBytes + usage.AudioCacheBytes;
            var downloads = usage.DownloadBytes + files.downloads;
            var library = usage.DatabaseBytes - usage.DatabaseCacheBytes + usage.OriginalBytes + files.intake;
            StorageTotal.Text = $"Зайнято {DisplaySize(library + cache + downloads)}";
            StorageBreakdown.Text = $"Бібліотека й база  {DisplaySize(library)}  ·  Кеш  {DisplaySize(cache)}  ·  Завантажені ресурси  {DisplaySize(downloads)}";
        }
        catch (Exception error) { StorageStatus.Text = error.Message; }
        finally
        {
            loadingStorage = false;
            StorageControls.IsEnabled = !changingStorage;
        }
    }

    private void BackgroundTranslationChanged(object? sender, ToggledEventArgs args)
    {
        if (loadingStorage || changingStorage) return;
        try
        {
            AppState.StartTranslationWorker(Dispatcher);
            SetBackgroundTranslation(args.Value);
            StorageStatus.Text = "";
        }
        catch (Exception error)
        {
            loadingStorage = true;
            BackgroundTranslationSwitch.IsToggled = !args.Value;
            loadingStorage = false;
            StorageStatus.Text = error.Message;
        }
    }

    private static void SetBackgroundTranslation(bool enabled)
    {
        AppState.Session.UpdateReaderPreference(ReaderPreferenceChange.BackgroundTranslation(enabled));
        AppState.TranslationWorker?.SetEnabled(enabled);
    }

    private bool ResourceOperationActive => activeDownload is not null || changingTranslationFiles
        || changingKnowledgePackage || knowledgeDownload is not null || optionalModelBusy
        || optionalDownload is not null || choosingLexical || lexicalCopy is not null || restoringBackup;

    private async void ClearStorageClicked(object? sender, EventArgs args)
    {
        if (changingStorage || sender is not Button { CommandParameter: string scope }) return;
        var (title, detail) = scope switch
        {
            "cache" => ("Очистити кеш?", "Збережені автоматичні переклади, мовні підказки й аудіо буде видалено. Фоновий переклад вимкнеться."),
            "downloads" => ("Видалити завантажені ресурси?", "Офлайн-моделі, мовні пакети й незавершені завантаження буде видалено. Книжки та словник залишаться в бібліотеці."),
            _ => ("Видалити всі локальні дані?", "Книжки, словник, нотатки, історію, налаштування та завантажені ресурси буде видалено. Цю дію неможливо скасувати."),
        };
        changingStorage = true;
        SettingsScroll.IsEnabled = false;
        var importGateHeld = false;
        try
        {
            if (ResourceOperationActive || AppState.Session.Busy)
            {
                StorageStatus.Text = "Зачекайте, поки завершиться поточна операція.";
                return;
            }
            if (!await DisplayAlertAsync(title, detail, "Видалити", "Скасувати")) return;
            importGateHeld = !DesktopImport.HasIncoming && await DesktopImport.Gate.WaitAsync(0);
            if (!importGateHeld)
            {
                StorageStatus.Text = "Зачекайте, поки завершиться імпорт файла.";
                return;
            }
            if (AppState.TranslationWorker is { } worker) await worker.PauseAsync();
            if (scope == "cache") SetBackgroundTranslation(false);
            var session = AppState.Session;
            try
            {
                await session.ClearStorageAsync(scope);
                var reset = scope == "all";
                if (scope != "downloads") AppState.InvalidateReaderCache();
                if (reset)
                {
                    AppState.TranslationWorker?.SetEnabled(false);
                    Preferences.Default.Clear();
                }
                if (scope != "cache") await Task.Run(() => RemoveAuxiliaryFiles(scope == "all"));
                StorageStatus.Text = "Готово.";
            }
            finally
            {
                if (scope == "all")
                {
                    await session.RefreshAsync();
                    (Shell.Current as AppShell)?.RefreshRecent();
                }
                if (scope != "cache")
                {
                    await RefreshTranslationPackagesAsync();
                    await RefreshKnowledgePackagesAsync();
                    await RefreshLexicalPackagesAsync();
                    await RefreshOptionalModelsAsync();
                }
            }
        }
        catch (Exception error) { StorageStatus.Text = error.Message; }
        finally
        {
            if (importGateHeld)
            {
                DesktopImport.Gate.Release();
                AppState.TranslationWorker?.Resume(clearProgress: true);
                await RefreshStorageAsync();
            }
            changingStorage = false;
            StorageControls.IsEnabled = true;
            SettingsScroll.IsEnabled = true;
        }
    }

    private static void RemoveAuxiliaryFiles(bool all)
    {
        foreach (var name in TemporaryDownloads.Concat(all ? ["exports"] : Array.Empty<string>()))
            RemoveFolder(Path.Combine(FileSystem.CacheDirectory, name));
        if (all)
            foreach (var name in LocalIntake)
                RemoveFolder(Path.Combine(FileSystem.AppDataDirectory, name));
    }

    private static void RemoveFolder(string path)
    {
        if (Directory.Exists(path)) Directory.Delete(path, recursive: true);
    }

    private static long FolderSize(string path)
    {
        if (!Directory.Exists(path)) return 0;
        var options = new EnumerationOptions { RecurseSubdirectories = true, AttributesToSkip = FileAttributes.ReparsePoint };
        return new DirectoryInfo(path).EnumerateFiles("*", options).Sum(file => file.Length);
    }

    private static string DisplaySize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} Б";
        if (bytes < 1024L * 1024) return $"{bytes / 1024d:0.#} КіБ";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024d * 1024):0.#} МіБ";
        return $"{bytes / (1024d * 1024 * 1024):0.##} ГіБ";
    }
}
