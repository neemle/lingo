using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private OptionalModel[] optionalModels = [];
    private bool optionalModelBusy;
    private CancellationTokenSource? optionalDownload;
    private static string OptionalStagingRoot => Path.Combine(FileSystem.CacheDirectory, "optional-model-downloads");

    private OptionalModel? SelectedOptionalModel
    {
        get
        {
            var index = OptionalModelChoice.SelectedIndex;
            return index >= 0 && index < optionalModels.Length ? optionalModels[index] : null;
        }
    }

    private async Task RefreshOptionalModelsAsync()
    {
        try
        {
            var previous = SelectedOptionalModel?.Id;
            optionalModels = await AppState.Session.OptionalModelsAsync();
            optionalInventoryRead = true;
            OptionalModelChoice.ItemsSource = optionalModels.Select(model =>
                $"{model.Name} · {model.Assets.Sum(asset => asset.Bytes) / 1_000_000d:F0} МБ · {model.License} · " +
                (model.FilesPresent ? "файли на місці" : model.HasLocalFiles ? "пошкоджені або неповні файли" : "доступно"))
                .ToArray();
            OptionalModelChoice.SelectedIndex = optionalModels.Length == 0 ? -1
                : Math.Max(0, Array.FindIndex(optionalModels, model => model.Id == previous));
            UpdateOptionalModelControls();
            UpdateCapabilityMatrix();
        }
        catch (Exception error)
        {
            optionalInventoryRead = false;
            OptionalModelStatus.Text = error.Message;
            UpdateCapabilityMatrix();
        }
    }

    private void OptionalModelChanged(object? sender, EventArgs args) => UpdateOptionalModelControls();

    private void UpdateOptionalModelControls()
    {
        var selected = SelectedOptionalModel;
        var memory = selected?.Memory;
        OptionalModelMemory.IsVisible = memory is not null;
        OptionalModelMemory.Text = memory switch
        {
            null => "",
            { TotalMib: null } => "Доступну пам’ять визначити не вдалося.",
            { TotalMib: long total, MinimumTotalMib: var minimum } when total < minimum =>
                $"Для запуску потрібно щонайменше {minimum} МіБ RAM.",
            { AvailableMib: long available, RequiredMib: var required } =>
                $"Пам’ять під час перевірки — {available} МіБ доступно, {required} МіБ потрібно для запуску.",
            _ => "Доступну пам’ять визначити не вдалося.",
        };
        OptionalModelChoice.IsEnabled = !optionalModelBusy;
        DownloadOptionalModel.IsEnabled = RemoveOptionalModel.IsEnabled = !optionalModelBusy && selected is not null;
        VerifyOptionalModel.IsEnabled = !optionalModelBusy && selected is { FilesPresent: true };
        DownloadOptionalModel.Text = selected is { HasLocalFiles: true } ? "Завантажити повторно" : "Завантажити";
    }

    private async void DownloadOptionalModelClicked(object? sender, EventArgs args)
    {
        if (optionalModelBusy || SelectedOptionalModel is not { } model) return;
        optionalModelBusy = true;
        UpdateOptionalModelControls();
        try
        {
            var bytes = model.Assets.Sum(asset => asset.Bytes);
            if (!await DisplayAlertAsync("Завантажити модель?",
                $"{model.Name} · {bytes / 1_000_000d:F0} МБ. Файли буде завантажено з Hugging Face.", "Завантажити", "Скасувати")) return;
            using var cancellation = new CancellationTokenSource();
            optionalDownload = cancellation;
            StopOptionalModel.IsVisible = OptionalProgress.IsVisible = true;
            OptionalProgress.Progress = 0;
            OptionalModelStatus.Text = "Перевірка вільного місця…";
            await AppState.Session.CheckOptionalModelSpaceAsync(model.Id, OptionalStagingRoot);
            cancellation.Token.ThrowIfCancellationRequested();

            OptionalModelStatus.Text = $"Завантаження {model.Name}…";
            var progress = new Progress<long>(received =>
            {
                if (ReferenceEquals(optionalDownload, cancellation)) OptionalProgress.Progress = (double)received / bytes;
            });
            await Task.Run(() => Downloader.FetchOptionalModelAsync(model, OptionalStagingRoot, progress, cancellation.Token),
                cancellation.Token);
            cancellation.Token.ThrowIfCancellationRequested();

            StopOptionalModel.IsVisible = false;
            OptionalModelStatus.Text = "Перевірка й встановлення…";
            await AppState.Session.InstallOptionalModelAsync(model.Id, OptionalStagingRoot);
            OptionalModelStatus.Text = $"Файли {model.Name} перевірено й встановлено.";
            await RefreshOptionalModelsAsync();
            ModelDownload.ClearOptionalStaging(model, OptionalStagingRoot);
        }
        catch (OperationCanceledException) when (optionalDownload?.IsCancellationRequested == true)
        {
            OptionalModelStatus.Text = "Завантаження зупинено. Виберіть цю модель ще раз, щоб продовжити.";
        }
        catch (Exception error) { OptionalModelStatus.Text = error.Message; }
        finally
        {
            optionalDownload = null;
            optionalModelBusy = false;
            StopOptionalModel.IsVisible = OptionalProgress.IsVisible = false;
            UpdateOptionalModelControls();
        }
    }

    private void StopOptionalModelClicked(object? sender, EventArgs args) => optionalDownload?.Cancel();

    private async void VerifyOptionalModelClicked(object? sender, EventArgs args)
    {
        if (optionalModelBusy || SelectedOptionalModel is not { } model) return;
        optionalModelBusy = true;
        UpdateOptionalModelControls();
        OptionalModelStatus.Text = "Перевірка файлів…";
        try
        {
            await AppState.Session.VerifyOptionalModelAsync(model.Id);
            OptionalModelStatus.Text = $"Контрольні суми {model.Name} збігаються.";
        }
        catch (Exception error) { OptionalModelStatus.Text = error.Message; }
        finally
        {
            optionalModelBusy = false;
            UpdateOptionalModelControls();
        }
    }

    private async void RemoveOptionalModelClicked(object? sender, EventArgs args)
    {
        if (optionalModelBusy || SelectedOptionalModel is not { } model) return;
        optionalModelBusy = true;
        UpdateOptionalModelControls();
        try
        {
            if (!await DisplayAlertAsync("Видалити файли моделі?",
                $"{model.Name}. Також буде видалено незавершене завантаження.", "Видалити", "Скасувати")) return;
            await AppState.Session.RemoveOptionalModelAsync(model.Id);
            ModelDownload.ClearOptionalStaging(model, OptionalStagingRoot);
            OptionalModelStatus.Text = $"Локальні файли {model.Name} видалено.";
        }
        catch (Exception error) { OptionalModelStatus.Text = error.Message; }
        finally
        {
            await RefreshOptionalModelsAsync();
            optionalModelBusy = false;
            UpdateOptionalModelControls();
        }
    }
}
