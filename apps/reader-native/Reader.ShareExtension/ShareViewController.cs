using Foundation;
using Reader.Native.Services;
using UIKit;
using UniformTypeIdentifiers;

namespace Reader.ShareExtension;

[Register("LingoShareViewController")]
public sealed class ShareViewController(ObjCRuntime.NativeHandle handle) : UIViewController(handle)
{
    private UILabel status = null!;
    private UIButton add = null!;
    private CancellationTokenSource? saving;

    public override void ViewDidLoad()
    {
        base.ViewDidLoad();
        var view = View!;
        view.BackgroundColor = UIColor.SystemBackground;
        status = new UILabel { Text = "Додати PDF або фото до «Читай»?", Lines = 0 };
        add = UIButton.FromType(UIButtonType.System);
        add.SetTitle("Додати до вхідних матеріалів", UIControlState.Normal);
        add.TouchUpInside += AddClicked;
        var cancel = UIButton.FromType(UIButtonType.System);
        cancel.SetTitle("Скасувати", UIControlState.Normal);
        cancel.TouchUpInside += (_, _) =>
        {
            if (saving is { } operation) operation.Cancel();
            else Complete();
        };
        var stack = new UIStackView([status, add, cancel])
        {
            Axis = UILayoutConstraintAxis.Vertical,
            Spacing = 16,
            TranslatesAutoresizingMaskIntoConstraints = false,
        };
        view.AddSubview(stack);
        var safe = view.SafeAreaLayoutGuide;
        NSLayoutConstraint.ActivateConstraints([
            stack.LeadingAnchor.ConstraintEqualTo(safe.LeadingAnchor, 24),
            stack.TrailingAnchor.ConstraintEqualTo(safe.TrailingAnchor, -24),
            stack.TopAnchor.ConstraintEqualTo(safe.TopAnchor, 24),
        ]);
    }

    private async void AddClicked(object? sender, EventArgs args)
    {
        if (saving is not null) return;
        using var operation = new CancellationTokenSource();
        saving = operation;
        add.Enabled = false;
        status.Text = "Копіюємо файл…";
        try
        {
            var context = ExtensionContext;
            var provider = (context?.InputItems.FirstOrDefault() as NSExtensionItem)?.Attachments?.FirstOrDefault()
                ?? throw new InvalidDataException("Відсутній PDF або файл зображення.");
            var type = provider.HasItemConformingTo(UTTypes.Pdf.Identifier) ? UTTypes.Pdf : UTTypes.Image;
            using var incoming = await OpenAsync(provider, type.Identifier, operation.Token);
            var root = SharedImportFolder.GetPath()
                ?? throw new IOException("Не вдалося відкрити спільну папку Читай.");
            await StoreAsync(root, incoming, operation.Token);
            Complete();
        }
        catch (OperationCanceledException) when (operation.IsCancellationRequested) { Complete(); }
        catch (Exception error)
        {
            status.Text = error.GetBaseException().Message;
            add.Enabled = true;
        }
        finally { saving = null; }
    }

    private void Complete() => ExtensionContext?.CompleteRequest([], null);

    private sealed record Incoming(string Name, FileStream Stream) : IDisposable
    {
        public void Dispose() => Stream.Dispose();
    }

    private static async Task<Incoming> OpenAsync(NSItemProvider provider, string type, CancellationToken cancellation)
    {
        var completion = new TaskCompletionSource<Incoming>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var progress = provider.LoadFileRepresentation(type, (url, error) =>
        {
            try
            {
                if (error is not null || url?.Path is not { } path)
                    throw new IOException(error?.LocalizedDescription ?? "Не вдалося відкрити файл.");
                var extension = Path.GetExtension(path).ToLowerInvariant();
                var title = Path.GetFileNameWithoutExtension(provider.SuggestedName);
                var name = (string.IsNullOrWhiteSpace(title) ? "Документ" : title) + extension;
                if (SharedImportFolder.Capacity(name) == 0)
                    throw new InvalidDataException("Цей формат файлу не підтримується.");
                var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read,
                    64 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
                if (!completion.TrySetResult(new Incoming(name, stream))) stream.Dispose();
            }
            catch (Exception failure) { completion.TrySetException(failure); }
        });
        using var registration = cancellation.Register(() =>
        {
            completion.TrySetCanceled(cancellation);
            progress.Cancel();
        });
        return await completion.Task;
    }

    private static async Task StoreAsync(string root, Incoming incoming, CancellationToken cancellation)
    {
        var limit = SharedImportFolder.Capacity(incoming.Name);
        var length = incoming.Stream.Length;
        if (length == 0 || length > limit)
            throw new InvalidDataException($"Максимальний розмір файлу — {limit / 1048576} МіБ.");
        if (System.Text.Encoding.UTF8.GetByteCount(incoming.Name) > SharedImportFolder.NameLimit)
            throw new InvalidDataException("Ім’я файлу задовге.");
        Directory.CreateDirectory(root);
        if (Directory.EnumerateDirectories(root).Where(path => Guid.TryParseExact(Path.GetFileName(path), "N", out _))
            .Take(16).Count() == 16)
            throw new IOException("Спільна черга заповнена. Відкрийте «Читай» і звільніть місце.");
        var id = Guid.NewGuid().ToString("N");
        var temporary = Path.Combine(root, "." + id);
        Directory.CreateDirectory(temporary);
        try
        {
            await using (var destination = File.Create(Path.Combine(temporary, SharedImportFolder.Payload)))
            {
                var buffer = new byte[64 * 1024];
                var remaining = length;
                while (remaining > 0)
                {
                    var chunk = buffer.AsMemory(0, (int)Math.Min(buffer.Length, remaining));
                    await incoming.Stream.ReadExactlyAsync(chunk, cancellation).ConfigureAwait(false);
                    await destination.WriteAsync(chunk, cancellation).ConfigureAwait(false);
                    remaining -= chunk.Length;
                }
                if (incoming.Stream.Length != length) throw new IOException("Файл змінився під час копіювання.");
                destination.Flush(flushToDisk: true);
            }
            await File.WriteAllTextAsync(Path.Combine(temporary, SharedImportFolder.Name), incoming.Name, cancellation)
                .ConfigureAwait(false);
            cancellation.ThrowIfCancellationRequested();
            Directory.Move(temporary, Path.Combine(root, id));
        }
        finally
        {
            if (Directory.Exists(temporary)) Directory.Delete(temporary, recursive: true);
        }
    }
}
