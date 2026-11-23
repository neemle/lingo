using Reader.Contracts;
using Reader.Native.Features.Reading;
using Reader.Presentation;

namespace Reader.Native.Services;

public static class AppState
{
    private static ReaderSession? session;
    internal static BackgroundTranslation? TranslationWorker { get; private set; }
    internal static int StorageRevision { get; private set; }
    internal static void InvalidateReaderCache() => StorageRevision++;

    internal static void StartTranslationWorker(IDispatcher dispatcher) =>
        TranslationWorker ??= new BackgroundTranslation(Session, dispatcher);
    public static ReaderSession Session => session ??=
        new ReaderSession(Path.Combine(FileSystem.AppDataDirectory, "reader-native.db"));

    public static void Close()
    {
        TranslationWorker?.Stop();
        TranslationWorker = null;
        session?.Dispose();
        session = null;
    }

    internal static void ForgetRecentDocument(string id)
    {
        if (Preferences.Get("lastDocument", "") == id) Preferences.Remove("lastDocument");
        var remaining = Preferences.Get("recentDocuments", "").Split(';', StringSplitOptions.RemoveEmptyEntries)
            .Where(value => value != id);
        Preferences.Set("recentDocuments", string.Join(';', remaining));
        (Shell.Current as AppShell)?.RefreshRecent();
    }

    public static async Task OpenAsync(string id, DictionarySourceLocation? source = null)
    {
        if (Session.Busy) return;
        if (source is null)
            await Session.SelectAsync(id);
        else
        {
            await Session.OpenDictionarySourceAsync(source);
            if (Session.Error.Length > 0) throw new InvalidOperationException(Session.Error);
        }
        if (Session.Error.Length > 0 || Session.Document?.Id != id) return;
        Preferences.Set("lastDocument", id);
        var recent = Preferences.Get("recentDocuments", "").Split(';').Prepend(id)
            .Where(value => value.Length != 0).Distinct().Take(3);
        Preferences.Set("recentDocuments", string.Join(";", recent));
        (Shell.Current as AppShell)?.RefreshRecent();
        var shell = Shell.Current;
        await shell.GoToAsync(AppShell.ReadingRoute);
        if (shell.CurrentPage is not ReadingPage reader) return;
        if (source is not null) await reader.RevealDictionarySourceAsync(source);
        else if (Session.Document is { Id: var selected, Format: "pdf" } && selected == id)
            await reader.ShowOriginalAsync();
    }
}
