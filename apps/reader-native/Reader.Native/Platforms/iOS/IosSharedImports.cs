namespace Reader.Native.Services;

internal static class IosSharedImports
{
    internal sealed record Received(List<ShareInboxItem> Items, List<(string Name, Exception Error)> Rejected);

    private static readonly SemaphoreSlim Gate = new(1);

    internal static async Task<Received?> ReceiveAsync()
    {
        if (!await Gate.WaitAsync(0).ConfigureAwait(false)) return null;
        var received = new Received([], []);
        using var incoming = DesktopImport.TrackIncoming();
        try
        {
            if (SharedImportFolder.GetPath() is not { } root || !Directory.Exists(root)) return null;
            foreach (var directory in Directory.EnumerateDirectories(root))
            {
                if (!Guid.TryParseExact(Path.GetFileName(directory), "N", out var id) ||
                    File.GetAttributes(directory).HasFlag(FileAttributes.ReparsePoint)) continue;
                var name = "Спільний матеріал";
                try
                {
                    var metadata = new FileInfo(Path.Combine(directory, SharedImportFolder.Name));
                    if (!metadata.Exists || metadata.Length is 0 or > SharedImportFolder.NameLimit ||
                        metadata.Attributes.HasFlag(FileAttributes.ReparsePoint))
                        throw new InvalidDataException("Пошкоджене ім’я спільного файлу.");
                    name = await File.ReadAllTextAsync(metadata.FullName).ConfigureAwait(false);
                    if (name != Path.GetFileName(name) || name.Any(char.IsControl) ||
                        SharedImportFolder.Capacity(name) == 0)
                        throw new InvalidDataException("Формат спільного файлу не підтримується.");
                    var path = Path.Combine(directory, SharedImportFolder.Payload);
                    if (File.GetAttributes(path).HasFlag(FileAttributes.ReparsePoint))
                        throw new InvalidDataException("Пошкоджена копія спільного файлу.");
                    ShareInboxItem item;
                    await using (var source = File.OpenRead(path))
                        item = await ShareInbox.StageAsync("file", name, source, stagedId: id).ConfigureAwait(false);
                    Directory.Delete(directory, recursive: true);
                    received.Items.Add(item);
                }
                catch (ShareInboxFullException) { break; }
                catch (Exception error) { received.Rejected.Add((name, error)); }
            }
        }
        catch (Exception error) { received.Rejected.Add(("Спільні матеріали", error)); }
        finally { Gate.Release(); }
        return received;
    }
}
