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
        status = new UILabel { Text = "Додати вибрані матеріали до «Читай»?", Lines = 0 };
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
        status.Text = "Зберігаємо матеріали…";
        try
        {
            var providers = ExtensionContext?.InputItems.OfType<NSExtensionItem>()
                .SelectMany(item => item.Attachments ?? []).ToArray() ?? [];
            if (providers.Length is 0 or > SharedImportFolder.MaxItems)
                throw new InvalidDataException($"Виберіть від 1 до {SharedImportFolder.MaxItems} матеріалів.");
            var root = SharedImportFolder.GetPath()
                ?? throw new IOException("Не вдалося відкрити спільну папку Читай.");
            await StoreBatchAsync(root, providers, operation.Token);
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

    private sealed record Incoming(string Name, Stream Stream, string Kind = "file") : IDisposable
    {
        public void Dispose() => Stream.Dispose();
    }

    private static async Task<Incoming> OpenItemAsync(NSItemProvider provider, CancellationToken cancellation)
    {
        string[] fileTypes = [UTTypes.Pdf.Identifier, SharedImportFolder.WordDocumentType, UTTypes.Image.Identifier];
        if (fileTypes.FirstOrDefault(provider.HasItemConformingTo) is { } fileType)
            return await OpenAsync(provider, fileType, cancellation).ConfigureAwait(false);
        var plainText = provider.HasItemConformingTo(UTTypes.PlainText.Identifier);
        if (provider.HasItemConformingTo(UTTypes.Url.Identifier) &&
            (!plainText || !provider.HasItemConformingTo(UTTypes.FileUrl.Identifier)))
        {
            using var url = await LoadObjectAsync<NSUrl>(provider, cancellation).ConfigureAwait(false);
            if (!Uri.TryCreate(url.AbsoluteString, UriKind.Absolute, out var address) ||
                address.Scheme is not ("http" or "https") || address.Host.Length == 0 ||
                address.UserInfo.Length > 0 || address.AbsoluteUri.Length > 4096)
                throw new InvalidDataException("Потрібне коректне HTTP або HTTPS посилання.");
            return FromValue("Вебпосилання", "url", address.AbsoluteUri);
        }
        if (!plainText)
            throw new InvalidDataException("Цей матеріал не підтримується.");
        try { return await OpenAsync(provider, UTTypes.PlainText.Identifier, cancellation).ConfigureAwait(false); }
        catch (IOException)
        {
            using var text = await LoadObjectAsync<NSString>(provider, cancellation).ConfigureAwait(false);
            return FromValue("Спільний текст", "text", text.ToString());
        }
    }

    private static async Task<T> LoadObjectAsync<T>(NSItemProvider provider, CancellationToken cancellation)
        where T : NSObject, INSItemProviderReading
    {
        var load = provider.LoadObjectAsync<T>(out var progress);
        using (progress)
        using (cancellation.Register(progress.Cancel))
            return await load.WaitAsync(cancellation).ConfigureAwait(false)
                ?? throw new InvalidDataException("Не вдалося прочитати спільний матеріал.");
    }

    private static Incoming FromValue(string name, string kind, string value)
    {
        if (string.IsNullOrWhiteSpace(value) ||
            System.Text.Encoding.UTF8.GetByteCount(value) > SharedImportFolder.Capacity(name, kind))
            throw new InvalidDataException("Спільний матеріал порожній або завеликий.");
        return new(name, new MemoryStream(System.Text.Encoding.UTF8.GetBytes(value), writable: false), kind);
    }

    private static async Task<Incoming> OpenAsync(NSItemProvider provider, string type, CancellationToken cancellation)
    {
        var completion = new TaskCompletionSource<Incoming>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var progress = provider.LoadFileRepresentation(type, (url, error) =>
        {
            FileStream? stream = null;
            try
            {
                if (error is not null || url?.Path is not { } path)
                    throw new IOException(error?.LocalizedDescription ?? "Не вдалося відкрити файл.");
                var extension = type == SharedImportFolder.WordDocumentType ? ".docx"
                    : type == UTTypes.PlainText.Identifier ? ".txt" : Path.GetExtension(path).ToLowerInvariant();
                var title = Path.GetFileName(provider.SuggestedName)?.Trim();
                if (string.IsNullOrWhiteSpace(title))
                {
                    title = Path.GetFileNameWithoutExtension(path);
                    if (Guid.TryParse(title, out _)) title = null;
                }
                else if (title.EndsWith(extension, StringComparison.OrdinalIgnoreCase) && extension.Length > 0)
                    title = title[..^extension.Length];
                var name = (string.IsNullOrWhiteSpace(title) ? "Документ" : title) + extension;
                if (SharedImportFolder.Capacity(name) == 0)
                    throw new InvalidDataException("Цей формат файлу не підтримується.");
                stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read,
                    64 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
                if (type == UTTypes.PlainText.Identifier && IsArchivedText(stream))
                {
                    var incoming = ReadArchivedText(stream);
                    if (!completion.TrySetResult(incoming)) incoming.Dispose();
                    return;
                }
                if (completion.TrySetResult(new Incoming(name, stream))) stream = null;
            }
            catch (Exception failure) { completion.TrySetException(failure); }
            finally { stream?.Dispose(); }
        });
        using var registration = cancellation.Register(() =>
        {
            completion.TrySetCanceled(cancellation);
            progress.Cancel();
        });
        return await completion.Task;
    }

    private static bool IsArchivedText(Stream stream)
    {
        // iOS can supply a keyed archive of NSString as public.plain-text.
        Span<byte> signature = stackalloc byte[8];
        var length = stream.ReadAtLeast(signature, signature.Length, false);
        stream.Position = 0;
        return length == signature.Length && signature.SequenceEqual("bplist00"u8);
    }

    private static Incoming ReadArchivedText(Stream stream)
    {
        const string name = "Спільний текст";
        const string kind = "text";
        if (stream.Length > SharedImportFolder.Capacity(name, kind))
            throw new InvalidDataException("Спільний текст завеликий.");
        using var data = NSData.FromStream(stream);
        if (data is null) throw new IOException("Не вдалося прочитати спільний текст.");
        using var value = NSKeyedUnarchiver.GetUnarchivedObject(typeof(NSString), data, out var error) as NSString;
        if (value is null) throw new IOException(error?.LocalizedDescription ?? "Не вдалося прочитати спільний текст.");
        return FromValue(name, kind, value.ToString());
    }

    private static async Task StoreBatchAsync(string root, NSItemProvider[] providers, CancellationToken cancellation)
    {
        Directory.CreateDirectory(root);
        EnsureCapacity();
        var id = Guid.NewGuid().ToString("N");
        var temporary = Path.Combine(root, "." + id);
        Directory.CreateDirectory(temporary);
        try
        {
            foreach (var provider in providers)
            {
                using var incoming = await OpenItemAsync(provider, cancellation).ConfigureAwait(false);
                await StoreAsync(Path.Combine(temporary, Guid.NewGuid().ToString("N")), incoming, cancellation)
                    .ConfigureAwait(false);
            }
            cancellation.ThrowIfCancellationRequested();
            using var publication = File.Open(Path.Combine(root, ".publish"), FileMode.OpenOrCreate,
                FileAccess.ReadWrite, FileShare.None);
            EnsureCapacity();
            Directory.Move(temporary, Path.Combine(root, SharedImportFolder.BatchPrefix + id));
        }
        finally
        {
            if (Directory.Exists(temporary)) Directory.Delete(temporary, recursive: true);
        }

        void EnsureCapacity()
        {
            var remaining = SharedImportFolder.MaxItems - providers.Length;
            if (SharedImportFolder.QueuedFiles(root).Take(remaining + 1).Count() > remaining)
                throw new IOException("Спільна черга заповнена. Відкрийте «Читай» і звільніть місце.");
        }
    }

    private static async Task StoreAsync(string directory, Incoming incoming, CancellationToken cancellation)
    {
        var limit = SharedImportFolder.Capacity(incoming.Name, incoming.Kind);
        var length = incoming.Stream.Length;
        if (length == 0 || length > limit)
            throw new InvalidDataException($"Максимальний розмір файлу — {limit / 1048576} МіБ.");
        if (System.Text.Encoding.UTF8.GetByteCount(incoming.Name) > SharedImportFolder.NameLimit)
            throw new InvalidDataException("Ім’я файлу задовге.");
        Directory.CreateDirectory(directory);
        await using (var destination = File.Create(Path.Combine(directory, SharedImportFolder.Payload)))
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
        await File.WriteAllTextAsync(Path.Combine(directory, SharedImportFolder.Name), incoming.Name, cancellation)
            .ConfigureAwait(false);
        if (incoming.Kind != "file")
            await File.WriteAllTextAsync(Path.Combine(directory, SharedImportFolder.Kind), incoming.Kind, cancellation)
                .ConfigureAwait(false);
    }
}
