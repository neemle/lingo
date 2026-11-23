using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private bool loadingStorage;
    private bool changingStorage;
    private BackgroundTranslation? monitoredWorker;
    private IDispatcherTimer? storageRefreshTimer;
    private Task? automaticStorageRefresh;

    private void StartStorageRefreshTimer()
    {
        if (storageRefreshTimer is null)
        {
            storageRefreshTimer = Dispatcher.CreateTimer();
            storageRefreshTimer.Interval = TimeSpan.FromSeconds(10);
            storageRefreshTimer.Tick += RefreshStorageOnTick;
        }
        storageRefreshTimer.Start();
    }

    private void RefreshStorageOnTick(object? sender, EventArgs args)
    {
        if (!settingsVisible || changingStorage || loadingStorage ||
            automaticStorageRefresh is { IsCompleted: false }) return;
        automaticStorageRefresh = RefreshStorageTotalsAsync(automatic: true);
    }

    private void MonitorBackgroundTranslation()
    {
        var worker = AppState.TranslationWorker;
        if (worker != monitoredWorker)
        {
            StopMonitoringBackgroundTranslation();
            monitoredWorker = worker;
            if (worker is not null) worker.StatusChanged += ShowBackgroundTranslationStatus;
        }
        ShowBackgroundTranslationStatus();
    }

    private void StopMonitoringBackgroundTranslation()
    {
        if (monitoredWorker is { } worker) worker.StatusChanged -= ShowBackgroundTranslationStatus;
        monitoredWorker = null;
    }

    private void ShowBackgroundTranslationStatus()
    {
        var status = monitoredWorker?.Status ?? "";
        BackgroundStatusText.Text = status;
        BackgroundStatus.IsVisible = status.Length > 0;
        BackgroundActivity.IsVisible = BackgroundActivity.IsRunning = monitoredWorker?.Running == true;
        BackgroundRequiredPackages.IsVisible = RequiredBackgroundPackages().Length > 0;
        var parked = monitoredWorker?.ParkedDocuments ?? 0;
        BackgroundRetryPanel.IsVisible = parked > 0;
        BackgroundRetryText.Text = $"Документів із призупиненим перекладом: {parked}.";
    }

    private void RetryBackgroundTranslationClicked(object? sender, EventArgs args) =>
        monitoredWorker?.RetryParkedDocuments();

    private TranslationPair[] RequiredBackgroundPackages() =>
        (monitoredWorker?.RequiredPackages ?? []).Where(pair => translationInventory is null ||
            translationInventory.Any(package => !package.FilesPresent &&
                package.Source == pair.Source && package.Target == pair.Target)).ToArray();

    private void OpenBackgroundPackagesClicked(object? sender, EventArgs args)
    {
        var missing = RequiredBackgroundPackages();
        if (missing.Length > 0) FocusTranslationPackages(missing);
    }

    private static readonly string[] TemporaryDownloads =
        ["model-transport", "translation-downloads", "knowledge-downloads", "lexical-downloads", "optional-model-downloads"];
    private static readonly string[] LocalIntake = ["share-inbox", "pdf-ocr"];

    private async void RefreshStorageClicked(object? sender, EventArgs args) => await RefreshStorageAsync();

    private async Task RefreshStorageAsync(bool syncWorker = false)
    {
        loadingStorage = true;
        StorageControls.IsEnabled = false;
        try
        {
            if (automaticStorageRefresh is { } pending) await pending;
            var session = AppState.Session;
            var preferences = session.GetReaderPreferences();
            var backgroundEnabled = preferences.BackgroundTranslation == true;
            BackgroundTranslationSwitch.IsToggled = backgroundEnabled;
            HyAutoTranslationSwitch.IsToggled = preferences.HyAutoTranslation == true;
            if (syncWorker) AppState.TranslationWorker?.SetEnabled(backgroundEnabled);
            await RefreshStorageTotalsAsync(automatic: false);
        }
        catch (Exception error) { StorageStatus.Text = error.Message; }
        finally
        {
            loadingStorage = false;
            StorageControls.IsEnabled = !changingStorage;
        }
    }

    private async Task RefreshStorageTotalsAsync(bool automatic)
    {
        try
        {
            var usage = await AppState.Session.StorageUsageAsync();
            var files = await Task.Run(() =>
            {
                var downloads = DownloadStaging().Sum(EntrySize);
                var intake = LocalIntake.Sum(name => FolderSize(Path.Combine(FileSystem.AppDataDirectory, name)));
                var exports = FolderSize(NativeFiles.ExportRoot);
                return (downloads, intake, exports);
            });
            if (automatic && (!settingsVisible || changingStorage || loadingStorage)) return;
            var cache = usage.DatabaseCacheBytes + usage.AudioCacheBytes + files.exports;
            var downloads = usage.DownloadBytes + files.downloads;
            var library = usage.DatabaseBytes - usage.DatabaseCacheBytes + usage.OriginalBytes + files.intake;
            StorageTotal.Text = $"Зайнято {DisplaySize(library + cache + downloads)}";
            LibraryStorage.Text = DisplaySize(library);
            CacheStorage.Text = DisplaySize(cache);
            TranslationCacheStorage.Text = DisplaySize(usage.TranslationCacheBytes);
            DownloadStorage.Text = DisplaySize(downloads);
        }
        catch (Exception error)
        {
            if (!automatic) throw;
            System.Diagnostics.Debug.WriteLine(error);
        }
    }

    private void BackgroundTranslationChanged(object? sender, ToggledEventArgs args)
    {
        if (loadingStorage || changingStorage) return;
        try
        {
            AppState.StartTranslationWorker(Dispatcher);
            MonitorBackgroundTranslation();
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

    private void HyAutoTranslationChanged(object? sender, ToggledEventArgs args)
    {
        if (loadingStorage || changingStorage) return;
        try
        {
            AppState.Session.UpdateReaderPreference(ReaderPreferenceChange.HyAutoTranslation(args.Value));
            AppState.TranslationWorker?.TranslationRoutesChanged();
            StorageStatus.Text = "";
        }
        catch (Exception error)
        {
            loadingStorage = true;
            HyAutoTranslationSwitch.IsToggled = !args.Value;
            loadingStorage = false;
            StorageStatus.Text = error.Message;
        }
    }

    private bool ResourceOperationActive => activeDownload is not null || changingTranslationFiles
        || changingKnowledgePackage || knowledgeDownload is not null || optionalModelBusy
        || optionalDownload is not null || choosingLexical || lexicalCopy is not null || restoringBackup
        || removingLanguageResource;

    private async void ClearStorageClicked(object? sender, EventArgs args)
    {
        if (changingStorage || sender is not Button { CommandParameter: string scope }) return;
        var (title, detail) = scope switch
        {
            "cache" => ("Очистити кеш?", "Збережені автоматичні переклади, мовні підказки, аудіо й тимчасові копії для поширення буде видалено. Фоновий переклад вимкнеться."),
            "downloads" => ("Видалити завантажені ресурси?", "Офлайн-моделі, мовні пакети, кеш модельних голосів і незавершені завантаження буде видалено. Книжки та словник залишаться в бібліотеці."),
            "all" => ("Видалити всі локальні дані?", "Застосунок створить порожню базу. Книжки, словник, нотатки, історію, налаштування та завантажені ресурси буде видалено. Цю дію неможливо скасувати."),
            _ => ("", ""),
        };
        if (title.Length == 0) return;
        changingStorage = true;
        SettingsScroll.IsEnabled = false;
        var importGateHeld = false;
        var inboxPaused = false;
        var exportsPaused = false;
        var storageOperationStarted = false;
        BackgroundTranslation? pausedWorker = null;
        try
        {
            if (ResourceOperationActive || AppState.Session.Busy)
            {
                StorageStatus.Text = "Зачекайте, поки завершиться поточна операція.";
                return;
            }
            if (!await DisplayAlertAsync(title, detail, "Видалити", "Скасувати")) return;
            if (automaticStorageRefresh is { } pending) await pending;
            if (ResourceOperationActive || AppState.Session.Busy)
            {
                StorageStatus.Text = "Зачекайте, поки завершиться поточна операція.";
                return;
            }
            importGateHeld = !DesktopImport.HasIncoming && await DesktopImport.Gate.WaitAsync(0);
            if (!importGateHeld)
            {
                StorageStatus.Text = "Зачекайте, поки завершиться імпорт файла.";
                return;
            }
            if (scope == "all")
            {
                inboxPaused = ShareInbox.TryPauseStaging();
                if (!inboxPaused || DesktopImport.HasIncoming)
                {
                    StorageStatus.Text = "Зачекайте, поки завершиться імпорт файла.";
                    return;
                }
            }
            if (scope != "downloads" && !(exportsPaused = NativeFiles.TryPauseExports()))
            {
                StorageStatus.Text = "Завершіть збереження або поширення файла й повторіть дію.";
                return;
            }
            StorageStatus.Text = "Видаляємо дані…";
            storageOperationStarted = true;
            if (AppState.TranslationWorker is { } worker)
            {
                pausedWorker = worker;
                await worker.PauseAsync();
            }
            var session = AppState.Session;
            if (scope != "downloads") AppState.InvalidateReaderCache();
            try
            {
                await session.ClearStorageAsync(scope);
                if (scope == "all")
                    Preferences.Default.Clear();
                await Task.Run(() => RemoveAuxiliaryFiles(scope));
                StorageStatus.Text = scope switch
                {
                    "cache" => "Кеш очищено. Фоновий переклад вимкнено.",
                    "downloads" => "Завантажені ресурси видалено.",
                    _ => "Усі локальні дані видалено."
                };
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
            if (exportsPaused) NativeFiles.ResumeExports();
            if (inboxPaused) ShareInbox.ResumeStaging();
            if (importGateHeld) DesktopImport.Gate.Release();
            try
            {
                if (storageOperationStarted) await RefreshStorageAsync(syncWorker: scope != "downloads");
            }
            finally
            {
                pausedWorker?.Resume(clearProgress: true);
                changingStorage = false;
                StorageControls.IsEnabled = true;
                SettingsScroll.IsEnabled = true;
            }
        }
    }

    private static void RemoveAuxiliaryFiles(string scope)
    {
        if (scope != "cache")
            foreach (var entry in DownloadStaging())
            {
                if (entry is DirectoryInfo directory) RemoveFolder(directory.FullName);
                else entry.Delete();
            }
        if (scope == "all")
            foreach (var name in LocalIntake)
                RemoveFolder(Path.Combine(FileSystem.AppDataDirectory, name));
        if (scope != "downloads") NativeFiles.ClearPausedExports();
    }

    private static IEnumerable<FileSystemInfo> DownloadStaging()
    {
        var root = new DirectoryInfo(FileSystem.CacheDirectory);
        if (!root.Exists) yield break;
        foreach (var entry in root.EnumerateFileSystemInfos())
        {
            var name = entry.Name;
            if (entry is DirectoryInfo && (TemporaryDownloads.Contains(name) ||
                name.StartsWith("optional-model-import-", StringComparison.Ordinal) &&
                Guid.TryParseExact(name["optional-model-import-".Length..], "N", out _)))
                yield return entry;
            if (entry is FileInfo && name.StartsWith("translation-", StringComparison.Ordinal) &&
                name.EndsWith(".zip", StringComparison.Ordinal) &&
                Guid.TryParseExact(name["translation-".Length..^".zip".Length], "N", out _))
                yield return entry;
        }
    }

    private static long EntrySize(FileSystemInfo entry)
    {
        try
        {
            if (entry.Attributes.HasFlag(FileAttributes.ReparsePoint)) return 0;
            return entry is FileInfo file ? file.Length : FolderSize(entry.FullName);
        }
        catch (FileNotFoundException) { return 0; }
        catch (DirectoryNotFoundException) { return 0; }
    }

    private static void RemoveFolder(string path)
    {
        var directory = new DirectoryInfo(path);
        if (directory.Exists)
            directory.Delete(recursive: !directory.Attributes.HasFlag(FileAttributes.ReparsePoint));
    }

    private static long FolderSize(string path)
    {
        if (!Directory.Exists(path)) return 0;
        var options = new EnumerationOptions { RecurseSubdirectories = true, AttributesToSkip = FileAttributes.ReparsePoint };
        try
        {
            var directory = new DirectoryInfo(path);
            return directory.Attributes.HasFlag(FileAttributes.ReparsePoint)
                ? 0 : directory.EnumerateFiles("*", options).Sum(EntrySize);
        }
        catch (DirectoryNotFoundException) { return 0; }
    }

    private static string DisplaySize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} Б";
        if (bytes < 1024L * 1024) return $"{bytes / 1024d:0.#} КіБ";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024d * 1024):0.#} МіБ";
        return $"{bytes / (1024d * 1024 * 1024):0.##} ГіБ";
    }
}
