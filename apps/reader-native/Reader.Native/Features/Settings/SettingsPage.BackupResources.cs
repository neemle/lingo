using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

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
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Архів Читай JSON" });
            if (file is null) return;
            Status.Text = "Копіюємо резервну копію…";
            directory = Directory.CreateTempSubdirectory("lingo-restore-");
            var path = Path.Combine(directory.FullName, "reader-backup.json");
            await using (var source = await file.OpenReadAsync())
            await using (var destination = new FileStream(path, FileMode.CreateNew, FileAccess.Write,
                FileShare.None, 64 * 1024, FileOptions.Asynchronous))
                await NativeFiles.CopyBoundedAsync(source, destination, Reader.CoreClient.CoreClient.MaxBackupFileBytes);

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
