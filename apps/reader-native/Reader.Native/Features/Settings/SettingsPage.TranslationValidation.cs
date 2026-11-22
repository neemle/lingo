using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private readonly List<TranslationPackageValidation> translationValidations = [];

    private static bool CanValidateTranslation(TranslationPackageStatus package) =>
        package.FilesPresent && package.Repository is not null && package.Source != package.Target
        && package.Source is "en" or "fr" or "uk" && package.Target is "en" or "fr" or "uk";

    private void InvalidateTranslationValidation(TranslationPackageStatus? package = null)
    {
        var removed = translationValidations.RemoveAll(result =>
            package is null || result.Repository == package.Repository);
        if (removed > 0) UpdateCapabilityMatrix();
    }

    private void InvalidateTranslationValidationForInstalledRoutes(string routes)
    {
        // The ZIP installer reports each activated direction as a language-code pair.
        var pairs = routes.Split(", ", StringSplitOptions.RemoveEmptyEntries);
        var installed = (translationInventory ?? []).Where(package =>
            pairs.Contains($"{package.Source} → {package.Target}")).ToArray();
        if (installed.Length == 0)
            InvalidateTranslationValidation();
        else
            foreach (var package in installed) InvalidateTranslationValidation(package);
    }

    private async void ValidateTranslationClicked(object? sender, EventArgs args)
    {
        if (changingTranslationFiles || activeDownload is not null) return;
        var choices = (translationInventory ?? []).Where(CanValidateTranslation)
            .ToDictionary(package => DescribeRoutes([package]));
        if (choices.Count == 0) return;

        changingTranslationFiles = true;
        UpdateTranslationControls();
        TranslationPackageStatus? selected = null;
        try
        {
            var choice = await DisplayActionSheetAsync("Пробний офлайн-переклад", "Скасувати", null, choices.Keys.ToArray());
            if (choice is null || !choices.TryGetValue(choice, out selected)) return;
            ModelStatus.Text = $"Виконуємо пробний офлайн-переклад ({choice})…";
            var result = await AppState.Session.ValidateTranslationPackageAsync(selected.Source, selected.Target);
            if (!result.Matches(selected))
                throw new InvalidDataException("Під час перевірки змінилася ревізія пакета.");

            translationValidations.RemoveAll(evidence => evidence.Matches(selected));
            translationValidations.Add(result);
            UpdateCapabilityMatrix();
            ModelStatus.Text = $"Пробний офлайн-переклад пройдено ({choice}).\n{result.Input}\n{result.Output}";
        }
        catch (Exception error)
        {
            if (selected is not null) InvalidateTranslationValidation(selected);
            ModelStatus.Text = error.Message;
        }
        finally
        {
            changingTranslationFiles = false;
            UpdateTranslationControls();
        }
    }
}
