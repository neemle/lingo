using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage : ContentPage
{
    public SettingsPage() { InitializeComponent(); BindingContext = AppState.Session; }

    private async void ExportClicked(object? sender, EventArgs args)
    {
        try
        {
            var backup = await AppState.Session.ExportAsync();
            await NativeFiles.ShareBytesAsync("reader-backup.json", NativeFiles.Encode(backup));
            Status.Text = "Архів підготовлено. Місце збереження обираєте ви.";
        }
        catch (Exception error) { await DisplayAlertAsync("Резервна копія", error.Message, "Гаразд"); }
    }

    private async void RestoreClicked(object? sender, EventArgs args)
    {
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Архів Читай JSON" });
            if (file is null) return;
            var bytes = await NativeFiles.ReadBoundedAsync(file, 256 * 1024 * 1024);
            var backup = await Task.Run(() => NativeFiles.Decode<Backup>(bytes));
            if (!await DisplayAlertAsync("Відновити архів?",
                "Документи й слова з архіву буде додано до локальної бібліотеки.", "Відновити", "Скасувати")) return;
            await AppState.Session.RestoreAsync(backup);
            Status.Text = AppState.Session.Error.Length == 0 ? "Архів відновлено." : "Відновлення не підтверджено.";
        }
        catch (Exception error) { await DisplayAlertAsync("Відновлення", error.Message, "Гаразд"); }
    }

    private async void OriginalClicked(object? sender, EventArgs args)
    {
        try
        {
            if (AppState.Session.Document is not { } doc) { Status.Text = "Спочатку відкрийте документ."; return; }
            var original = await AppState.Session.OriginalAsync(doc.Id);
            var bytes = original.Bytes.Select(value => checked((byte)value)).ToArray();
            await NativeFiles.ShareBytesAsync(original.Name, bytes);
        }
        catch (Exception error) { await DisplayAlertAsync("Оригінал", error.Message, "Гаразд"); }
    }

    private async void InstallTranslationClicked(object? sender, EventArgs args)
    {
        InstallTranslation.IsEnabled = false;
        var path = Path.Combine(FileSystem.CacheDirectory, $"translation-{Guid.NewGuid():N}.zip");
        try
        {
            var file = await FilePicker.Default.PickAsync(new PickOptions { PickerTitle = "Мовний пакет ZIP" });
            if (file is null) return;
            Status.Text = "Перевірка мовного пакета…";
            await using (var input = await file.OpenReadAsync())
            await using (var output = File.Create(path))
            {
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
        }
        catch (Exception error) { Status.Text = error.Message; }
        finally
        {
            try { File.Delete(path); }
            catch (IOException) { Status.Text += " Не вдалося видалити тимчасову копію архіву."; }
            catch (UnauthorizedAccessException) { Status.Text += " Не вдалося видалити тимчасову копію архіву."; }
            finally { InstallTranslation.IsEnabled = true; }
        }
    }

    private async void ProbeClicked(object? sender, EventArgs args) =>
        await Navigation.PushAsync(new ProbePage());
}
