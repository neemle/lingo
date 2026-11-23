using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private bool restoringBackup;

    private async void RestoreClicked(object? sender, EventArgs args)
    {
        if (restoringBackup) return;
        restoringBackup = true;
        RestoreBackup.IsEnabled = false;
        DirectoryInfo? directory = null;
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Резервна копія Читай (ZIP або JSON)" });
            if (file is null) return;
            Status.Text = "Копіюємо резервну копію…";
            directory = Directory.CreateTempSubdirectory("lingo-restore-");
            var path = Path.Combine(directory.FullName, "library.backup");
            await StageBackupAsync(file, path);

            Status.Text = "Перевіряємо оригінали в архіві…";
            var session = AppState.Session;
            var preview = await session.InspectBackupFileAsync(path);
            var summary = $"Документи — {preview.Documents}\n" +
                $"Словникові картки — {preview.Vocabulary}\n" +
                $"Повторення — {preview.Reviews}\n" +
                $"Сеанси читання — {preview.ReadingSessions}\n" +
                $"Мовні ресурси в маніфесті — {preview.Resources}";
            if (preview.HasPractice) summary += "\nЄ збережене запитання практики.";
            if (preview.Documents > 0) summary += "\nКонтрольні суми оригіналів збігаються.";
            summary += "\n\nДані архіву буде об’єднано з локальною бібліотекою.";
            Status.Text = "";
            if (!await DisplayAlertAsync("Відновити архів?", summary, "Відновити", "Скасувати")) return;

            Status.Text = "Відновлюємо архів…";
            var resources = await session.RestoreBackupFileAsync(path);
            Status.Text = resources is null ? "Відновлення не підтверджено." : "Архів відновлено.";
            if (resources is not null)
            {
                AppState.TranslationWorker?.TranslationRoutesChanged();
                await AppState.RefreshRecentReadingHistoryAsync();
                (Shell.Current as AppShell)?.RefreshRecent();
                await RefreshStorageAsync(syncWorker: true);
            }
            if (resources is { Length: > 0 }) await ReportBackupResourcesAsync(resources);
        }
        catch (Exception error)
        {
            Status.Text = "";
            await DisplayAlertAsync("Відновлення", error.Message, "Гаразд");
        }
        finally
        {
            restoringBackup = false;
            RestoreBackup.IsEnabled = true;
            try { directory?.Delete(recursive: true); }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                Status.Text += "\nНе вдалося видалити тимчасову копію архіву.";
            }
        }
    }

    private async Task StageBackupAsync(FileResult file, string path)
    {
        await using var source = await file.OpenReadAsync();
        var prefix = new byte[4];
        var length = await source.ReadAtLeastAsync(prefix, prefix.Length, false);
        var isZip = length == prefix.Length && prefix.AsSpan().SequenceEqual("PK\u0003\u0004"u8);
        // ZIP originals are streamed and checked individually by the core. Legacy JSON is decoded in memory.
        var maximum = isZip ? long.MaxValue : Client.MaxBackupFileBytes;
        await using var destination = new FileStream(path, FileMode.CreateNew, FileAccess.Write,
            FileShare.None, 64 * 1024, FileOptions.Asynchronous);
        await destination.WriteAsync(prefix.AsMemory(0, length));
        var progress = new Progress<long>(bytes =>
        {
            if (restoringBackup && Status.Text?.StartsWith("Копіюємо резервну копію", StringComparison.Ordinal) == true)
                Status.Text = $"Копіюємо резервну копію · {(bytes + length) / (1024d * 1024):F0} МіБ";
        });
        await NativeFiles.CopyBoundedAsync(source, destination, maximum - length, progress: progress);
    }

    private async Task ReportBackupResourcesAsync(BackupResource[] resources)
    {
        try
        {
            var translations = await AppState.Session.TranslationPackagesAsync();
            var optional = await AppState.Session.OptionalModelsAsync();
            var grammar = await AppState.Session.KnowledgePackagesAsync();
            var lexical = await AppState.Session.LexicalPackagesAsync();
            var missing = BackupResourceReport.Missing(resources, translations, optional, grammar, lexical);
            if (missing.Length == 0) return;
            Status.Text += "\nПотрібні ресурси з архіву:\n" + string.Join("\n", missing.Take(32));
            if (missing.Length > 32) Status.Text += $"\nЩе ресурсів: {missing.Length - 32}.";
            Status.Text += "\nВстановіть їх з локальних файлів або завантажте у відповідному розділі налаштувань.";
        }
        catch (Exception error)
        {
            Status.Text += $"\nРесурси не перевірено: {error.Message}";
        }
    }
}
