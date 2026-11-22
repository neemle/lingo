using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage : ContentPage, IQueryAttributable
{
    private static readonly ModelDownload Downloader = new(new HttpClient(new HttpClientHandler
    {
        UseCookies = false,
    }));
    private TranslationPackageStatus[] downloadChoices = [];
    private IGrouping<string, TranslationPackageStatus>[] removablePackages = [];
    private TranslationPair[] requestedPackages = [];
    private CancellationTokenSource? activeDownload;
    private bool changingTranslationFiles;
    private bool changingKnowledgePackage;
    private CancellationTokenSource? knowledgeDownload;
    private KnowledgePackage? pausedKnowledgePackage;
    private bool settingsVisible;
    private bool settingsLoaded;
    private bool focusTranslationPending;

    public SettingsPage() { InitializeComponent(); BindingContext = AppState.Session; }

    public void ApplyQueryAttributes(IDictionary<string, object> query)
    {
        if (query.TryGetValue("translationPackages", out var value) && value is TranslationPair[] packages)
        {
            requestedPackages = packages.Distinct().ToArray();
            focusTranslationPending = requestedPackages.Length > 0;
            var index = RequestedPackageIndex();
            if (index >= 0) ModelChoice.SelectedIndex = index;
            UpdateTranslationControls();
            TranslationLayoutChanged(this, EventArgs.Empty);
        }
    }

    private int RequestedPackageIndex() => requestedPackages.Select(pair => Array.FindIndex(downloadChoices,
        package => package.Source == pair.Source && package.Target == pair.Target)).FirstOrDefault(index => index >= 0, -1);

    private static TranslationPackageStatus[] RequestedMissingPackages(TranslationPair[] pairs,
        TranslationPackageStatus[] inventory) => pairs.SelectMany(pair => inventory.Where(package =>
            !package.FilesPresent && package.Source == pair.Source && package.Target == pair.Target)).ToArray();

    private static string PackageKey(TranslationPackageStatus package) =>
        package.Repository ?? $"{package.Source}-{package.Target}";

    private void TranslationLayoutChanged(object? sender, EventArgs args)
    {
        if (!focusTranslationPending || !settingsVisible || !settingsLoaded ||
            SettingsScroll.Width <= 0 || TranslationSection.Height <= 0)
            return;
        focusTranslationPending = false;
        Dispatcher.Dispatch(async () =>
        {
            if (!settingsVisible) return;
            try { await SettingsScroll.ScrollToAsync(TranslationSection, ScrollToPosition.Start, animated: false); }
            catch (Exception error) { ModelStatus.Text = error.Message; }
        });
    }

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        settingsVisible = true;
        settingsLoaded = false;
        nativeCapabilities = null;
        capabilityReport = null;
        ExportCapabilities.IsEnabled = false;
        await RefreshTranslationPackagesAsync();
        await RefreshKnowledgePackagesAsync();
        await RefreshLexicalPackagesAsync();
        await RefreshOptionalModelsAsync();
        nativeCapabilities = await NativeLanguageCapabilities.ReadAsync();
        UpdateCapabilityMatrix();
        settingsLoaded = true;
        TranslationLayoutChanged(this, EventArgs.Empty);
    }

    protected override void OnDisappearing()
    {
        settingsVisible = false;
        focusTranslationPending = false;
        base.OnDisappearing();
    }

    private async Task RefreshKnowledgePackagesAsync()
    {
        try
        {
            var packages = await AppState.Session.KnowledgePackagesAsync();
            knowledgeInventory = packages;
            KnowledgePackages.Text = string.Join(Environment.NewLine, packages.Select(package =>
                $"{DocumentLanguages.Name(package.Language)} · {package.Name} {package.Version} · {package.License} · " +
                (package.SyntaxAvailable && package.ExpressionsAvailable ? "встановлено" : package.Installed
                    ? "доступне оновлення індексу" : "доступно для завантаження")));
            InstallKnowledge.IsVisible = packages.Any(package => !package.SyntaxAvailable || !package.ExpressionsAvailable);
            UpdateCapabilityMatrix();
        }
        catch (Exception error)
        {
            knowledgeInventory = null;
            KnowledgeStatus.Text = error.Message;
            UpdateCapabilityMatrix();
        }
    }

    private async void InstallKnowledgeClicked(object? sender, EventArgs args) =>
        await InstallKnowledgeAsync(download: true);

    private async void OpenKnowledgeClicked(object? sender, EventArgs args) =>
        await InstallKnowledgeAsync(download: false);

    private async void ResumeKnowledgeClicked(object? sender, EventArgs args)
    {
        if (pausedKnowledgePackage is { } package)
            await InstallKnowledgeAsync(download: true, resume: package);
    }

    private void PauseKnowledgeClicked(object? sender, EventArgs args)
    {
        if (knowledgeDownload is not { } download) return;
        PauseKnowledge.IsEnabled = false;
        download.Cancel();
    }

    private async Task InstallKnowledgeAsync(bool download, KnowledgePackage? resume = null)
    {
        if (changingKnowledgePackage) return;
        changingKnowledgePackage = true;
        InstallKnowledge.IsEnabled = OpenKnowledge.IsEnabled = ResumeKnowledge.IsEnabled = false;
        string? cleanupPath = null;
        try
        {
            var selected = resume ?? await ChooseKnowledgePackageAsync(download);
            if (selected is null) return;

            var root = Path.Combine(FileSystem.CacheDirectory, "knowledge-downloads");
            string path;
            if (download)
            {
                var downloaded = await DownloadKnowledgeAsync(selected, root);
                if (downloaded is null) return;
                path = downloaded;
            }
            else
            {
                var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = $"Файл .conllu для {selected.Name}" });
                if (file is null) return;
                Directory.CreateDirectory(root);
                path = Path.Combine(root, $"{Guid.NewGuid():N}.conllu");
                cleanupPath = path;
                KnowledgeStatus.Text = $"Копіювання файлу для {selected.Name}…";
                await using var input = await file.OpenReadAsync();
                await using var output = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None,
                    64 * 1024, FileOptions.Asynchronous);
                await NativeFiles.CopyBoundedAsync(input, output, checked((int)selected.Bytes));
            }

            KnowledgeStatus.Text = "Створення локального індексу…";
            await AppState.Session.InstallKnowledgePackageAsync(selected.Language, path);
            cleanupPath = path;
            if (pausedKnowledgePackage?.Revision == selected.Revision)
                pausedKnowledgePackage = null;
            KnowledgeStatus.Text = $"Встановлено {selected.Name}. Аналіз слів працює офлайн.";
            await RefreshKnowledgePackagesAsync();
        }
        catch (Exception error) { KnowledgeStatus.Text = error.Message; }
        finally
        {
            if (cleanupPath is not null)
            {
                try { File.Delete(cleanupPath); }
                catch (IOException) { KnowledgeStatus.Text += " Не вдалося видалити тимчасову копію."; }
                catch (UnauthorizedAccessException) { KnowledgeStatus.Text += " Не вдалося видалити тимчасову копію."; }
            }
            changingKnowledgePackage = false;
            InstallKnowledge.IsEnabled = OpenKnowledge.IsEnabled = true;
            ResumeKnowledge.IsVisible = ResumeKnowledge.IsEnabled = pausedKnowledgePackage is not null;
            if (pausedKnowledgePackage is { } paused)
                ResumeKnowledge.Text = $"Продовжити · {DocumentLanguages.Name(paused.Language)}";
        }
    }

    private async Task<KnowledgePackage?> ChooseKnowledgePackageAsync(bool download)
    {
        var packages = await AppState.Session.KnowledgePackagesAsync();
        var choices = packages.Where(package => !download || !package.SyntaxAvailable || !package.ExpressionsAvailable)
            .ToDictionary(package =>
                $"{DocumentLanguages.Name(package.Language)} · {package.License} · {package.Bytes / 1_000_000d:F0} МБ");
        if (choices.Count == 0) return null;
        var choice = await DisplayActionSheetAsync("Пакети Universal Dependencies", "Скасувати", null, choices.Keys.ToArray());
        return choice is not null && choices.TryGetValue(choice, out var selected) ? selected : null;
    }

    private async Task<string?> DownloadKnowledgeAsync(KnowledgePackage package, string root)
    {
        using var cancellation = new CancellationTokenSource();
        knowledgeDownload = cancellation;
        KnowledgeStatus.Text = $"Завантаження {package.Name}…";
        KnowledgeProgress.Progress = 0;
        PauseKnowledge.IsEnabled = PauseKnowledge.IsVisible = KnowledgeProgress.IsVisible = true;
        var progress = new Progress<long>(bytes =>
        {
            if (knowledgeDownload == cancellation)
                KnowledgeProgress.Progress = Math.Clamp((double)bytes / package.Bytes, 0, 1);
        });
        try
        {
            var path = await Task.Run(() => Downloader.FetchKnowledgeAsync(package, root, cancellation.Token, progress));
            cancellation.Token.ThrowIfCancellationRequested();
            return path;
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            pausedKnowledgePackage = package;
            KnowledgeStatus.Text = $"Завантаження {package.Name} призупинено.";
            return null;
        }
        finally
        {
            knowledgeDownload = null;
            PauseKnowledge.IsVisible = KnowledgeProgress.IsVisible = false;
        }
    }

    private async Task RefreshTranslationPackagesAsync()
    {
        try
        {
            var packages = await AppState.Session.TranslationPackagesAsync();
            translationInventory = packages;
            TranslationPackages.Text = string.Join(Environment.NewLine, packages.Select(package =>
                $"{DocumentLanguages.Name(package.Source)} → {DocumentLanguages.Name(package.Target)}: " +
                (package.HasValidatedTranslation ? "пробний офлайн-переклад пройдено"
                    : package.FilesPresent ? "файли на місці"
                    : package.HasLocalFiles ? "лишилися файли пакета" : "пакет відсутній")));
            var previous = ModelChoice.SelectedIndex >= 0 && ModelChoice.SelectedIndex < downloadChoices.Length
                ? downloadChoices[ModelChoice.SelectedIndex] : null;
            downloadChoices = packages.Where(package => !package.FilesPresent).ToArray();
            removablePackages = packages.Where(package => package.HasLocalFiles)
                .GroupBy(PackageKey).ToArray();
            ModelChoice.ItemsSource = downloadChoices.Select(package =>
                $"{DocumentLanguages.Name(package.Source)} → {DocumentLanguages.Name(package.Target)} " +
                $"({package.Assets.Sum(asset => asset.Bytes) / 1_000_000d:F0} МБ)").ToArray();
            var requested = RequestedPackageIndex();
            ModelChoice.SelectedIndex = downloadChoices.Length == 0 ? -1 : requested >= 0 ? requested
                : Math.Max(0, Array.FindIndex(downloadChoices, package => package.Source == previous?.Source &&
                    package.Target == previous?.Target));
            UpdateTranslationControls();
            UpdateCapabilityMatrix();
        }
        catch (Exception error)
        {
            translationInventory = null;
            TranslationPackages.Text = error.Message;
            UpdateTranslationControls();
            UpdateCapabilityMatrix();
        }
    }

    private void UpdateTranslationControls()
    {
        var busy = activeDownload is not null || changingTranslationFiles;
        var requested = RequestedMissingPackages(requestedPackages, translationInventory ?? []);
        RequestedTranslation.IsVisible = requested.Length > 0;
        RequestedTranslation.Text = requested.Length > 0 ? $"Потрібні пакети для напрямків {DescribeRoutes(requested)}." : "";
        DownloadRequested.IsVisible = requested.DistinctBy(PackageKey).Count() > 1;
        DownloadRequested.IsEnabled = !busy && DownloadRequested.IsVisible;
        ModelChoice.IsEnabled = InstallTranslation.IsEnabled = !busy;
        DownloadTranslation.IsEnabled = !busy && downloadChoices.Length > 0;
        RemoveTranslation.IsVisible = removablePackages.Length > 0;
        RemoveTranslation.IsEnabled = !busy && RemoveTranslation.IsVisible;
        VerifyTranslation.IsVisible = RemoveTranslation.IsVisible;
        VerifyTranslation.IsEnabled = !busy && VerifyTranslation.IsVisible;
        ValidateTranslation.IsVisible = translationInventory?.Any(CanValidateTranslation) == true;
        ValidateTranslation.IsEnabled = !busy && ValidateTranslation.IsVisible;
        RepairTranslation.IsVisible = RemoveTranslation.IsVisible;
        RepairTranslation.IsEnabled = !busy && RepairTranslation.IsVisible;
    }

    private async void ExportClicked(object? sender, EventArgs args)
    {
        try
        {
            Status.Text = "Готуємо резервну копію…";
            Status.Text = await NativeFiles.ExportAsync(this, "reader-backup.json",
                share: sender is Button { CommandParameter: "share" },
                write: AppState.Session.ExportBackupFileAsync) ?? "";
        }
        catch (Exception error)
        {
            Status.Text = "";
            await DisplayAlertAsync("Резервна копія", error.Message, "Гаразд");
        }
    }

    private async void RestoreClicked(object? sender, EventArgs args)
    {
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Архів Читай JSON" });
            if (file is null) return;
            if (!await DisplayAlertAsync("Відновити архів?",
                "Документи й слова з архіву буде додано до локальної бібліотеки.", "Відновити", "Скасувати")) return;
            Status.Text = "Відновлюємо архів…";
            var resources = await RestoreArchiveAsync(file);
            Status.Text = resources is null ? "Відновлення не підтверджено." : "Архів відновлено.";
            if (resources is { Length: > 0 }) await ReportBackupResourcesAsync(resources);
        }
        catch (Exception error)
        {
            Status.Text = "";
            await DisplayAlertAsync("Відновлення", error.Message, "Гаразд");
        }
    }

    private static async Task<BackupResource[]?> RestoreArchiveAsync(FileResult file)
    {
        var directory = Directory.CreateTempSubdirectory("lingo-restore-");
        try
        {
            var path = Path.Combine(directory.FullName, "reader-backup.json");
            await using (var source = await file.OpenReadAsync())
            await using (var destination = new FileStream(path, FileMode.CreateNew, FileAccess.Write,
                FileShare.None, 64 * 1024, FileOptions.Asynchronous))
                await NativeFiles.CopyBoundedAsync(source, destination, Reader.CoreClient.CoreClient.MaxBackupFileBytes);
            return await AppState.Session.RestoreBackupFileAsync(path);
        }
        finally { directory.Delete(recursive: true); }
    }

    private async void OriginalClicked(object? sender, EventArgs args)
    {
        try
        {
            if (AppState.Session.Document is not { } doc) { Status.Text = "Спочатку відкрийте документ."; return; }
            var original = await AppState.Session.OriginalAsync(doc.Id);
            var bytes = original.Bytes.Select(value => checked((byte)value)).ToArray();
            Status.Text = await NativeFiles.ExportBytesAsync(this, original.Name, bytes,
                share: sender is Button { CommandParameter: "share" }) ?? "";
        }
        catch (Exception error) { await DisplayAlertAsync("Оригінал", error.Message, "Гаразд"); }
    }

    private async void DownloadTranslationClicked(object? sender, EventArgs args)
    {
        var index = ModelChoice.SelectedIndex;
        if (index >= 0 && index < downloadChoices.Length) await DownloadPackageAsync(downloadChoices[index]);
    }

    private async void DownloadRequestedClicked(object? sender, EventArgs args) =>
        await RunTranslationDownloadAsync(requestedPackages, single: null);

    private async void VerifyTranslationClicked(object? sender, EventArgs args)
    {
        if (changingTranslationFiles || activeDownload is not null || removablePackages.Length == 0) return;
        changingTranslationFiles = true;
        UpdateTranslationControls();
        string? selectedRoutes = null;
        try
        {
            var choices = removablePackages.ToDictionary(group => DescribeRoutes(group), group => group.First());
            var choice = await DisplayActionSheetAsync("Перевірити цілісність пакета", "Скасувати", null, choices.Keys.ToArray());
            if (choice is null || !choices.TryGetValue(choice, out var package)) return;

            selectedRoutes = choice;
            ModelStatus.Text = $"Перевіряємо контрольні суми SHA-256 пакета ({choice})…";
            await AppState.Session.VerifyTranslationPackageAsync(package.Source, package.Target);
            ModelStatus.Text = $"Контрольні суми SHA-256 пакета ({choice}) збігаються.";
        }
        catch (Exception error)
        {
            if (selectedRoutes is not null) await RefreshTranslationPackagesAsync();
            ModelStatus.Text = selectedRoutes is null
                ? error.Message
                : $"Не вдалося перевірити пакет ({selectedRoutes}). {error.Message} Щоб відновити файли, виберіть «Завантажити пакет повторно».";
        }
        finally
        {
            changingTranslationFiles = false;
            UpdateTranslationControls();
        }
    }

    private async void RepairTranslationClicked(object? sender, EventArgs args)
    {
        if (changingTranslationFiles || activeDownload is not null || removablePackages.Length == 0) return;
        changingTranslationFiles = true;
        UpdateTranslationControls();
        TranslationPackageStatus? selected = null;
        try
        {
            var choices = removablePackages.ToDictionary(group =>
                $"{DescribeRoutes(group)} ({group.First().Assets.Sum(asset => asset.Bytes) / 1_000_000d:F0} МБ)",
                group => group.First());
            var choice = await DisplayActionSheetAsync("Завантажити пакет повторно", "Скасувати", null, choices.Keys.ToArray());
            if (choice is not null) choices.TryGetValue(choice, out selected);
        }
        catch (Exception error) { ModelStatus.Text = error.Message; }
        finally
        {
            changingTranslationFiles = false;
            UpdateTranslationControls();
        }
        if (selected is not null) await DownloadPackageAsync(selected);
    }

    private Task DownloadPackageAsync(TranslationPackageStatus package) => RunTranslationDownloadAsync([], package);

    private async Task RunTranslationDownloadAsync(TranslationPair[] pairs, TranslationPackageStatus? single)
    {
        if (changingTranslationFiles || activeDownload is not null) return;
        using var cancellation = new CancellationTokenSource();
        activeDownload = cancellation;
        UpdateTranslationControls();
        try
        {
            if (single is not null)
            {
                await InstallDownloadedPackageAsync(single, cancellation);
                return;
            }

            var installed = new HashSet<string>(StringComparer.Ordinal);
            await RefreshTranslationPackagesAsync();
            while (true)
            {
                cancellation.Token.ThrowIfCancellationRequested();
                var inventory = translationInventory ?? throw new InvalidOperationException(TranslationPackages.Text);
                var package = RequestedMissingPackages(pairs, inventory).FirstOrDefault();
                if (package is null) break;
                if (!installed.Add(PackageKey(package)))
                    throw new InvalidDataException($"Встановлення пакета {DescribeRoutes([package])} не підтверджено.");
                await InstallDownloadedPackageAsync(package, cancellation);
            }
            ModelStatus.Text = "Усі потрібні мовні пакети встановлено.";
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            ModelStatus.Text = "Завантаження зупинено. Виберіть цей пакет ще раз, щоб продовжити.";
        }
        catch (Exception error) { ModelStatus.Text = error.Message; }
        finally
        {
            activeDownload = null;
            CancelDownload.IsVisible = DownloadProgress.IsVisible = false;
            UpdateTranslationControls();
        }
    }

    private async Task InstallDownloadedPackageAsync(TranslationPackageStatus package, CancellationTokenSource cancellation)
    {
        var root = Path.Combine(FileSystem.CacheDirectory, "translation-downloads");
        var archive = Path.Combine(FileSystem.CacheDirectory, $"translation-{Guid.NewGuid():N}.zip");
        var total = package.Assets.Sum(asset => asset.Bytes);
        var progress = new Progress<long>(bytes =>
        {
            if (activeDownload == cancellation)
                DownloadProgress.Progress = Math.Clamp((double)bytes / total, 0, 1);
        });

        CancelDownload.IsVisible = DownloadProgress.IsVisible = true;
        DownloadProgress.Progress = 0;
        ModelStatus.Text = "Перевірка вільного місця…";
        try
        {
            await AppState.Session.CheckTranslationDownloadSpaceAsync(package.Source, package.Target, root, FileSystem.CacheDirectory);
            cancellation.Token.ThrowIfCancellationRequested();
            ModelStatus.Text = $"Завантаження пакета {DescribeRoutes([package])}…";
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
        finally { TryClean(() => File.Delete(archive)); }
    }

    private void CancelDownloadClicked(object? sender, EventArgs args) => activeDownload?.Cancel();

    private void TryClean(Action cleanup)
    {
        try { cleanup(); }
        catch (IOException) { ModelStatus.Text += " Не вдалося видалити тимчасові файли."; }
        catch (UnauthorizedAccessException) { ModelStatus.Text += " Не вдалося видалити тимчасові файли."; }
    }

    private static string DescribeRoutes(IEnumerable<TranslationPackageStatus> packages) =>
        string.Join(", ", packages.Select(package =>
            $"{DocumentLanguages.Name(package.Source)} → {DocumentLanguages.Name(package.Target)}"));

    private async void RemoveTranslationClicked(object? sender, EventArgs args)
    {
        if (changingTranslationFiles || activeDownload is not null || removablePackages.Length == 0) return;
        changingTranslationFiles = true;
        UpdateTranslationControls();
        try
        {
            var choices = removablePackages.ToDictionary(group => DescribeRoutes(group), group => group.First());
            var choice = await DisplayActionSheetAsync("Видалити пакет перекладу", "Скасувати", null, choices.Keys.ToArray());
            if (choice is null || !choices.TryGetValue(choice, out var package)) return;
            if (!await DisplayAlertAsync($"Видалити {choice}?",
                "Пакет можна буде завантажити знову. Ваші тексти й переклади збережуться.", "Видалити", "Скасувати"))
                return;

            var removed = await AppState.Session.RemoveTranslationPackageAsync(package.Source, package.Target);
            ModelStatus.Text = removed ? $"Пакет видалено ({choice})." : "Пакет уже відсутній.";
            var root = Path.Combine(FileSystem.CacheDirectory, "translation-downloads");
            if (Directory.Exists(root)) TryClean(() => ModelDownload.ClearStaging(package, root));
        }
        catch (Exception error) { ModelStatus.Text = error.Message; }
        finally
        {
            changingTranslationFiles = false;
            await RefreshTranslationPackagesAsync();
            UpdateTranslationControls();
        }
    }

    private async void InstallTranslationClicked(object? sender, EventArgs args)
    {
        if (changingTranslationFiles || activeDownload is not null) return;
        changingTranslationFiles = true;
        UpdateTranslationControls();
        var path = Path.Combine(FileSystem.CacheDirectory, $"translation-{Guid.NewGuid():N}.zip");
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Мовний пакет ZIP" });
            if (file is null) return;
            Status.Text = "Перевірка мовного пакета…";
            await using (var input = await file.OpenReadAsync())
            {
                if (input.CanSeek)
                    await AppState.Session.CheckTranslationArchiveSpaceAsync(FileSystem.CacheDirectory, input.Length);
                await using var output = File.Create(path);
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
                changingTranslationFiles = false;
                UpdateTranslationControls();
            }
        }
    }

    private async void ProbeClicked(object? sender, EventArgs args) =>
        await Navigation.PushAsync(new ProbePage());
}
