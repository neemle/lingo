using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private LexicalPackage[] lexicalPackages = [];
    private CancellationTokenSource? lexicalCopy;

    private async Task RefreshLexicalPackagesAsync()
    {
        try
        {
            var previousLanguage = SelectedLexicalPackage()?.Language;
            lexicalPackages = await AppState.Session.LexicalPackagesAsync();
            lexicalInventoryRead = true;
            LexicalChoice.ItemsSource = lexicalPackages.Select(package =>
                $"{DocumentLanguages.Name(package.Language)} · {package.Bytes / 1_000_000d:F0} МБ · " +
                (package.Installed ? "встановлено" : DocumentLanguages.Name(package.GlossLanguage) + " у визначеннях"))
                .ToArray();
            LexicalChoice.SelectedIndex = lexicalPackages.Length == 0 ? -1 :
                Math.Max(0, Array.FindIndex(lexicalPackages, package => package.Language == previousLanguage));
            UpdateLexicalControls();
            UpdateCapabilityMatrix();
        }
        catch (Exception error)
        {
            lexicalInventoryRead = false;
            LexicalStatus.Text = error.Message;
            UpdateCapabilityMatrix();
        }
    }

    private LexicalPackage? SelectedLexicalPackage() =>
        LexicalChoice.SelectedIndex >= 0 && LexicalChoice.SelectedIndex < lexicalPackages.Length
            ? lexicalPackages[LexicalChoice.SelectedIndex] : null;

    private void UpdateLexicalControls()
    {
        var enabled = lexicalCopy is null && lexicalPackages.Length > 0;
        LexicalChoice.IsEnabled = DownloadLexical.IsEnabled = OpenLexical.IsEnabled = enabled;
    }

    private async void DownloadLexicalClicked(object? sender, EventArgs args) => await InstallLexicalAsync(download: true);
    private async void OpenLexicalClicked(object? sender, EventArgs args) => await InstallLexicalAsync(download: false);
    private void StopLexicalClicked(object? sender, EventArgs args) => lexicalCopy?.Cancel();

    private async Task InstallLexicalAsync(bool download)
    {
        if (lexicalCopy is not null || SelectedLexicalPackage() is not { } package) return;
        using var cancellation = new CancellationTokenSource();
        lexicalCopy = cancellation;
        UpdateLexicalControls();
        string? temporary = null;
        try
        {
            var root = Path.Combine(FileSystem.CacheDirectory, "lexical-downloads");
            string path;
            var progress = new Progress<long>(length =>
            {
                if (lexicalCopy == cancellation && LexicalProgress.IsVisible)
                    LexicalProgress.Progress = Math.Clamp((double)length / package.Bytes, 0, 1);
            });
            if (download)
            {
                var name = DocumentLanguages.Name(package.Language);
                if (!await DisplayAlertAsync("Завантажити словник?",
                    $"{name} · {package.Bytes / 1_000_000d:F0} МБ · {package.License}", "Завантажити", "Скасувати")) return;
                ShowLexicalTransfer("Завантаження словника…");
                path = await Task.Run(() => Downloader.FetchLexicalAsync(package, root, progress, cancellation.Token));
            }
            else
            {
                var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Виберіть словниковий архів .jsonl.gz" });
                if (file is null) return;
                Directory.CreateDirectory(root);
                temporary = Path.Combine(root, Guid.NewGuid().ToString("N") + ".jsonl.gz");
                ShowLexicalTransfer("Читання словникового архіву…");
                await CopyLexicalAsync(file, temporary, package.Bytes, progress, cancellation.Token);
                path = temporary;
            }
            cancellation.Token.ThrowIfCancellationRequested();
            StopLexical.IsVisible = LexicalProgress.IsVisible = false;
            LexicalStatus.Text = "Створення локального словникового індексу…";
            await AppState.Session.InstallLexicalPackageAsync(package.Language, path);
            await RefreshLexicalPackagesAsync();
            LexicalStatus.Text = $"Словник встановлено · {DocumentLanguages.Name(package.Language)}.";
            if (download) RemoveLexicalCopy(path);
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            LexicalStatus.Text = "Перенесення зупинено.";
        }
        catch (Exception error) { LexicalStatus.Text = error.Message; }
        finally
        {
            if (temporary is not null) RemoveLexicalCopy(temporary);
            lexicalCopy = null;
            StopLexical.IsVisible = LexicalProgress.IsVisible = false;
            UpdateLexicalControls();
        }
    }

    private void ShowLexicalTransfer(string status)
    {
        LexicalStatus.Text = status;
        LexicalProgress.Progress = 0;
        LexicalProgress.IsVisible = StopLexical.IsVisible = true;
    }

    private void RemoveLexicalCopy(string path)
    {
        try { File.Delete(path); }
        catch (IOException) { LexicalStatus.Text += " Тимчасовий архів збережено в кеші."; }
        catch (UnauthorizedAccessException) { LexicalStatus.Text += " Тимчасовий архів збережено в кеші."; }
    }

    private static async Task CopyLexicalAsync(FileResult file, string path, long expected,
        IProgress<long> progress, CancellationToken cancellation)
    {
        await using var input = await file.OpenReadAsync();
        await using var output = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None,
            256 * 1024, FileOptions.Asynchronous);
        var buffer = new byte[256 * 1024];
        long copied = 0, reported = 0;
        int count;
        while ((count = await input.ReadAsync(buffer, cancellation)) != 0)
        {
            copied += count;
            if (copied > expected) throw new InvalidDataException("Розмір архіву не збігається з мовним пакетом.");
            await output.WriteAsync(buffer.AsMemory(0, count), cancellation);
            if (copied - reported >= 4_000_000)
            {
                progress.Report(copied);
                reported = copied;
            }
        }
        if (copied != expected) throw new InvalidDataException("Розмір архіву не збігається з мовним пакетом.");
        progress.Report(copied);
    }
}
