using Reader.Native.Services;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private async void ExportTranslationClicked(object? sender, EventArgs args)
    {
        if (changingTranslationFiles || activeDownload is not null) return;
        var choices = (translationInventory ?? []).Where(package => package.FilesPresent)
            .GroupBy(PackageKey).ToDictionary(group => DescribeRoutes(group), group => group.First());
        if (choices.Count == 0) return;

        changingTranslationFiles = true;
        UpdateTranslationControls();
        try
        {
            var choice = await DisplayActionSheetAsync("Зберегти пакет перекладу", "Скасувати", null, choices.Keys.ToArray());
            if (choice is null || !choices.TryGetValue(choice, out var package)) return;
            var name = $"lingo-{package.Source}-{package.Target}.zip";
            ModelStatus.Text = $"Перевіряємо й пакуємо {choice}…";
            ModelStatus.Text = await NativeFiles.ExportAsync(this, name, share: false,
                path => AppState.Session.ExportTranslationPackageAsync(package.Source, package.Target, path)) ?? "";
        }
        catch (Exception error) { ModelStatus.Text = error.Message; }
        finally
        {
            changingTranslationFiles = false;
            UpdateTranslationControls();
        }
    }
}
