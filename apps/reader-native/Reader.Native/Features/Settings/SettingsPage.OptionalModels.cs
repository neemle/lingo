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
        OpenOptionalModel.IsEnabled = !optionalModelBusy && optionalInventoryRead && selected is not null;
        VerifyOptionalModel.IsEnabled = !optionalModelBusy && selected is { HasLocalFiles: true };
        RollbackOptionalModel.IsEnabled = !optionalModelBusy && selected is { HasRecoveryCopy: true };
        RepairOptionalModel.IsEnabled = !optionalModelBusy && selected is not null
            && Directory.Exists(Path.Combine(OptionalStagingRoot, selected.Revision));
        OptionalModelRecovery.Text = selected is { HasRecoveryCopy: true }
            ? "Є збережена локальна копія. Перед відновленням буде перевірено її контрольні суми."
            : "Збереженої копії немає. Незавершене завантаження можна продовжити або встановити з кешу.";
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
            StopOptionalModel.Text = "Зупинити завантаження";
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
            OptionalModelStatus.Text = "Перевірка файлів і контрольний запуск…";
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

    private async void OpenOptionalModelClicked(object? sender, EventArgs args)
    {
        if (optionalModelBusy || SelectedOptionalModel is not { } model) return;
        optionalModelBusy = true;
        UpdateOptionalModelControls();
        var staging = Path.Combine(FileSystem.CacheDirectory, $"optional-model-import-{Guid.NewGuid():N}");
        try
        {
            var files = (await FilePicker.Default.PickMultipleAsync(new PickOptions
            {
                PickerTitle = $"Виберіть усі файли для «{model.Name}»",
            })).OfType<FileResult>().ToArray();
            if (files.Length == 0) return;

            using var cancellation = new CancellationTokenSource();
            optionalDownload = cancellation;
            StopOptionalModel.Text = "Зупинити копіювання";
            StopOptionalModel.IsVisible = OptionalProgress.IsVisible = true;
            OptionalProgress.Progress = 0;
            OptionalModelStatus.Text = "Перевірка вільного місця…";
            var session = AppState.Session;
            await session.CheckOptionalModelSpaceAsync(model.Id, staging);
            var sources = files.Select(file => (file.FileName, (Func<Task<Stream>>)file.OpenReadAsync)).ToArray();
            var total = model.Assets.Sum(asset => asset.Bytes);
            var progress = new Progress<long>(bytes =>
            {
                if (ReferenceEquals(optionalDownload, cancellation)) OptionalProgress.Progress = (double)bytes / total;
            });
            OptionalModelStatus.Text = $"Копіюємо локальні файли {model.Name}…";
            await ModelDownload.StageOptionalModelFilesAsync(model, staging, sources, progress, cancellation.Token);
            cancellation.Token.ThrowIfCancellationRequested();

            StopOptionalModel.IsVisible = false;
            OptionalModelStatus.Text = "Перевірка файлів і контрольний запуск…";
            await session.InstallOptionalModelAsync(model.Id, staging);
            OptionalModelStatus.Text = $"Локальні файли {model.Name} перевірено й встановлено.";
        }
        catch (OperationCanceledException) when (optionalDownload?.IsCancellationRequested == true)
        {
            OptionalModelStatus.Text = "Копіювання зупинено.";
        }
        catch (Exception error) { OptionalModelStatus.Text = error.Message; }
        finally
        {
            optionalDownload = null;
            StopOptionalModel.IsVisible = OptionalProgress.IsVisible = false;
            await RefreshOptionalModelsAsync();
            optionalModelBusy = false;
            UpdateOptionalModelControls();
            try { if (Directory.Exists(staging)) Directory.Delete(staging, recursive: true); }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                OptionalModelStatus.Text += "\nНе вдалося видалити тимчасові копії файлів.";
            }
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

    private async void RollbackOptionalModelClicked(object? sender, EventArgs args)
    {
        if (optionalModelBusy || SelectedOptionalModel is not { } model) return;
        await RecoverOptionalModelAsync(model, rollback: true);
    }

    private async void RepairOptionalModelClicked(object? sender, EventArgs args)
    {
        if (optionalModelBusy || SelectedOptionalModel is not { } model) return;
        await RecoverOptionalModelAsync(model, rollback: false);
    }

    private async Task RecoverOptionalModelAsync(OptionalModel model, bool rollback)
    {
        optionalModelBusy = true;
        UpdateOptionalModelControls();
        try
        {
            var action = rollback ? "Відновити збережену копію?" : "Встановити модель з локального кешу?";
            if (!await DisplayAlertAsync(action,
                $"{model.Name}. Файли буде перевірено без завантаження з мережі. Поточну встановлену копію буде збережено.",
                "Відновити", "Скасувати")) return;
            OptionalModelStatus.Text = "Перевірка локальних файлів і відновлення…";
            if (rollback) await AppState.Session.RollbackOptionalModelAsync(model.Id);
            else await AppState.Session.InstallOptionalModelAsync(model.Id, OptionalStagingRoot);
            OptionalModelStatus.Text = $"{model.Name}: локальну копію перевірено й відновлено.";
        }
        catch (Exception error) { OptionalModelStatus.Text = error.Message; }
        finally
        {
            await RefreshOptionalModelsAsync();
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
                $"{model.Name}. Буде видалено також збережені копії для відновлення й незавершене завантаження.", "Видалити", "Скасувати")) return;
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
