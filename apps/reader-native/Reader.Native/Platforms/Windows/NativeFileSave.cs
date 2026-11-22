using Windows.Storage.Pickers;

namespace Reader.Native.Services;

internal static class NativeFileSave
{
    internal static Task<bool> SaveAsync(Microsoft.Maui.Controls.Window window, string path)
    {
        return MainThread.InvokeOnMainThreadAsync(async () =>
        {
            var nativeWindow = window.Handler?.PlatformView as Microsoft.UI.Xaml.Window
                ?? throw new InvalidOperationException("Вікно ще не готове до збереження файлу.");
            var fileName = Path.GetFileName(path);
            var extension = Path.GetExtension(fileName);
            if (string.IsNullOrEmpty(extension)) extension = ".bin";
            var picker = new FileSavePicker
            {
                SuggestedFileName = fileName,
                DefaultFileExtension = extension,
                SuggestedStartLocation = PickerLocationId.Downloads
            };
            picker.FileTypeChoices.Add("Файл", new[] { extension });
            var handle = WinRT.Interop.WindowNative.GetWindowHandle(nativeWindow);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, handle);
            var file = await picker.PickSaveFileAsync();
            if (file is null) return false;
            if (string.Equals(file.Path, path, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Виберіть розташування за межами тимчасової теки.");

            await using var source = File.OpenRead(path);
            using var transaction = await file.OpenTransactedWriteAsync();
            await using var destination = transaction.Stream.AsStreamForWrite();
            destination.SetLength(0);
            await source.CopyToAsync(destination);
            await destination.FlushAsync();
            await transaction.CommitAsync();
            return true;
        });
    }
}
