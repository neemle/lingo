using Foundation;
using PhotosUI;
using UniformTypeIdentifiers;

namespace Reader.Native.Services;

internal static class ApplePhotoPicker
{
    internal sealed record Photo(string Name, FileStream Content);

    internal static async Task<Photo?> PickAsync()
    {
        var presenter = Platform.GetCurrentUIViewController()
            ?? throw new InvalidOperationException("Вікно застосунку недоступне.");
        using var configuration = new PHPickerConfiguration
        {
            Filter = PHPickerFilter.ImagesFilter,
            SelectionLimit = 1,
            PreferredAssetRepresentationMode = PHPickerConfigurationAssetRepresentationMode.Current,
        };
        using var picker = new PHPickerViewController(configuration) { ModalInPresentation = true };
        using var selection = new Selection();
        picker.Delegate = selection;
        presenter.PresentViewController(picker, animated: true, completionHandler: null);
        return await selection.Completion.Task;
    }

    private sealed class Selection : PHPickerViewControllerDelegate
    {
        internal TaskCompletionSource<Photo?> Completion { get; } =
            new(TaskCreationOptions.RunContinuationsAsynchronously);

        public override void DidFinishPicking(PHPickerViewController picker, PHPickerResult[] results)
        {
            picker.DismissViewController(animated: true, () =>
            {
                if (results.FirstOrDefault()?.ItemProvider is not { } provider)
                {
                    Completion.TrySetResult(null);
                    return;
                }
                provider.LoadFileRepresentation(UTTypes.Image.Identifier, (url, error) =>
                {
                    try
                    {
                        if (error is not null || url?.Path is not { } path)
                            throw new IOException(error?.LocalizedDescription ?? "Не вдалося відкрити фото.");
                        var extension = Path.GetExtension(path).ToLowerInvariant();
                        if (extension is not (".jpg" or ".jpeg" or ".png" or ".webp" or ".heic" or ".heif"))
                            throw new InvalidDataException("Формат фото не підтримується.");
                        var title = Path.GetFileNameWithoutExtension(provider.SuggestedName);
                        var name = (string.IsNullOrWhiteSpace(title) ? "Фото" : title) + extension;
                        // Photos removes the temporary path after this callback. The open descriptor keeps the bytes readable.
                        var content = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read,
                            64 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
                        if (!Completion.TrySetResult(new Photo(name, content))) content.Dispose();
                    }
                    catch (Exception failure) { Completion.TrySetException(failure); }
                });
            });
        }
    }
}
