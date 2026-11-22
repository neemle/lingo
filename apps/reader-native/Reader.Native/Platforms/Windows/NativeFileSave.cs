using Windows.Storage;
using Windows.Storage.Pickers;

namespace Reader.Native.Services;

internal static class NativeFileSave
{
    internal static Task<bool> SaveAsync(Microsoft.Maui.Controls.Window window, string name, byte[] bytes)
    {
        ArgumentNullException.ThrowIfNull(window);
        ArgumentException.ThrowIfNullOrWhiteSpace(name);
        ArgumentNullException.ThrowIfNull(bytes);

        return MainThread.InvokeOnMainThreadAsync(async () =>
        {
            var nativeWindow = window.Handler?.PlatformView as Microsoft.UI.Xaml.Window
                ?? throw new InvalidOperationException("Вікно ще не готове до збереження файлу.");
            var fileName = Path.GetFileName(name);
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

            await FileIO.WriteBytesAsync(file, bytes);
            return true;
        });
    }
}
