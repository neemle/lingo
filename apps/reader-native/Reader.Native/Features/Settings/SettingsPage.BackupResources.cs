using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
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
