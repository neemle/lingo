using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private OptionalModel[] optionalModels = [];
    private bool optionalModelBusy;
    private CancellationTokenSource? optionalDownload;
    private ModelTransferPlan? optionalTransfer;
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
            var hadHy = HasAutomaticHyModel;
            optionalModels = (await AppState.Session.OptionalModelsAsync())
                .Where(model => model.Id != "qwen-0.8b" || model.HasLocalFiles).ToArray();
            if (hadHy != HasAutomaticHyModel && AppState.Session.GetReaderPreferences().HyAutoTranslation == true)
                AppState.TranslationWorker?.TranslationRoutesChanged();
            optionalInventoryRead = true;
            OptionalModelChoice.ItemsSource = optionalModels.Select(model =>
                $"{model.Name} · {model.Assets.Sum(asset => asset.Bytes) / 1_000_000d:F0} МБ · {model.License} · " +
                (model.FilesPresent ? "файли на місці" : model.HasLocalFiles ? "пошкоджені або неповні файли" : "доступно"))
                .ToArray();
            OptionalModelChoice.SelectedIndex = optionalModels.Length == 0 ? -1
                : Math.Max(0, Array.FindIndex(optionalModels, model => model.Id == previous));
            UpdateOptionalModelControls();
            await RefreshOptionalTransferAsync();
            UpdateCapabilityMatrix();
        }
        catch (Exception error)
        {
            optionalInventoryRead = false;
            OptionalModelStatus.Text = error.Message;
            UpdateCapabilityMatrix();
        }
    }

    private bool HasAutomaticHyModel => optionalModels.Any(model => model.Id == "hy-mt2-1.8b" && model.FilesPresent);

    private async void OptionalModelChanged(object? sender, EventArgs args)
    {
        UpdateOptionalModelControls();
        await RefreshOptionalTransferAsync();
    }

    private async Task RefreshOptionalTransferAsync()
    {
        if (SelectedOptionalModel is not { } model || model.Id == "qwen-0.8b") return;
        try
        {
            var state = await AppState.Session.ModelTransferStatusAsync(ModelTransferPackage.Optional(model.Id));
            if (SelectedOptionalModel?.Id == model.Id) optionalTransfer = state;
        }
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error.Message);
        }
        UpdateOptionalModelControls();
    }

    private void UpdateOptionalModelControls()
    {
        var selected = SelectedOptionalModel;
        var memory = selected?.Memory;
        var retired = selected?.Id == "qwen-0.8b";
        OptionalModelMemory.IsVisible = memory is not null && !retired;
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
        var canInstall = selected is not null && !retired;
        RemoveOptionalModel.IsEnabled = !optionalModelBusy && selected is not null;
        DownloadOptionalModel.IsEnabled = !optionalModelBusy && canInstall;
        OpenOptionalModel.IsEnabled = !optionalModelBusy && optionalInventoryRead && canInstall;
        ExportOptionalModel.IsEnabled = !optionalModelBusy && canInstall && selected is { FilesPresent: true };
        VerifyOptionalModel.IsEnabled = !optionalModelBusy && canInstall && selected is { HasLocalFiles: true };
        RollbackOptionalModel.IsEnabled = !optionalModelBusy && canInstall && selected is { HasRecoveryCopy: true };
        var localTransfer = selected is not null && optionalTransfer?.Revision == selected.Revision
            ? optionalTransfer : null;
        RepairOptionalModel.IsEnabled = !optionalModelBusy && canInstall && selected is not null
            && (localTransfer is { TotalBytes: > 0 } && localTransfer.TotalBytes == localTransfer.ReceivedBytes
                || Directory.Exists(Path.Combine(OptionalStagingRoot, selected.Revision)));
        OptionalModelRecovery.Text = retired
            ? "Підтримку Qwen 0.8B припинено. Тут можна видалити її локальні файли."
            : selected is { HasRecoveryCopy: true }
            ? "Є збережена локальна копія. Перед відновленням буде перевірено її контрольні суми."
            : "Збереженої копії немає. Незавершене завантаження можна продовжити або встановити з кешу.";
        DownloadOptionalModel.Text = localTransfer is { Status: "paused" or "awaitingRange" or "readyToInstall" }
            ? "Продовжити" : selected is { HasLocalFiles: true } ? "Завантажити повторно" : "Завантажити";
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
            var session = AppState.Session;
            var transfer = ModelTransferPackage.Optional(model.Id);
            OptionalModelStatus.Text = $"Завантаження {model.Name}…";
            var progress = new Progress<long>(received =>
            {
                if (ReferenceEquals(optionalDownload, cancellation)) OptionalProgress.Progress = (double)received / bytes;
            });
            await Downloader.FetchModelAsync(session, transfer, ModelTransportDirectory, OptionalStagingRoot,
                progress, cancellation.Token);
            cancellation.Token.ThrowIfCancellationRequested();

            StopOptionalModel.IsVisible = false;
            OptionalModelStatus.Text = "Перевірка файлів і контрольний запуск…";
            await session.ActivateModelTransferAsync(transfer);
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
            await RefreshOptionalTransferAsync();
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
                PickerTitle = $"Виберіть ZIP або всі файли для «{model.Name}»",
            })).OfType<FileResult>().ToArray();
            if (files.Length == 0) return;

            using var cancellation = new CancellationTokenSource();
            optionalDownload = cancellation;
            StopOptionalModel.Text = "Зупинити копіювання";
            StopOptionalModel.IsVisible = OptionalProgress.IsVisible = true;
            OptionalProgress.Progress = 0;
            OptionalModelStatus.Text = "Перевірка вільного місця…";
            var session = AppState.Session;
            var total = model.Assets.Sum(asset => asset.Bytes);
            var progress = new Progress<long>(bytes =>
            {
                if (ReferenceEquals(optionalDownload, cancellation))
                    OptionalProgress.Progress = Math.Min(1, (double)bytes / total);
            });
            if (files.Length == 1 && Path.GetExtension(files[0].FileName).Equals(".zip", StringComparison.OrdinalIgnoreCase))
            {
                var archive = Path.Combine(staging, "model.zip");
                Directory.CreateDirectory(staging);
                OptionalModelStatus.Text = $"Копіюємо архів {model.Name}…";
                await using (var input = await files[0].OpenReadAsync())
                await using (var output = File.Create(archive))
                    await NativeFiles.CopyBoundedAsync(input, output, checked((int)(total + 1024 * 1024)),
                        cancellation.Token, progress);

                BeginActivation();
                await session.InstallOptionalModelArchiveAsync(model.Id, archive);
            }
            else
            {
                await session.CheckOptionalModelSpaceAsync(model.Id, staging);
                var sources = files.Select(file => (file.FileName, (Func<Task<Stream>>)file.OpenReadAsync)).ToArray();
                OptionalModelStatus.Text = $"Копіюємо локальні файли {model.Name}…";
                await ModelDownload.StageOptionalModelFilesAsync(model, staging, sources, progress, cancellation.Token);
                BeginActivation();
                await session.InstallOptionalModelAsync(model.Id, staging);
            }

            void BeginActivation()
            {
                cancellation.Token.ThrowIfCancellationRequested();
                StopOptionalModel.IsVisible = false;
                OptionalModelStatus.Text = "Перевірка файлів і контрольний запуск…";
            }
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

    private async void ExportOptionalModelClicked(object? sender, EventArgs args)
    {
        if (optionalModelBusy || SelectedOptionalModel is not { FilesPresent: true } model) return;
        optionalModelBusy = true;
        UpdateOptionalModelControls();
        try
        {
            OptionalModelStatus.Text = $"Перевіряємо й пакуємо {model.Name}…";
            var name = $"{model.Id}-{model.Revision[..8]}.zip";
            OptionalModelStatus.Text = await NativeFiles.ExportAsync(this, name, share: false,
                path => AppState.Session.ExportOptionalModelArchiveAsync(model.Id, path)) ?? "";
        }
        catch (Exception error) { OptionalModelStatus.Text = error.Message; }
        finally
        {
            optionalModelBusy = false;
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
            var session = AppState.Session;
            if (rollback) await session.RollbackOptionalModelAsync(model.Id);
            else
            {
                var transfer = ModelTransferPackage.Optional(model.Id);
                var state = await session.ModelTransferStatusAsync(transfer);
                if (state.ReceivedBytes > 0 && state.ReceivedBytes == state.TotalBytes)
                {
                    await session.ResumeModelTransferAsync(transfer);
                    await session.ActivateModelTransferAsync(transfer);
                }
                else await session.InstallOptionalModelAsync(model.Id, OptionalStagingRoot);
            }
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
            await AppState.Session.DiscardModelTransferAsync(ModelTransferPackage.Optional(model.Id));
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
