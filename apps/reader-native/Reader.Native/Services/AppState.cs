using Reader.Contracts;
using Reader.Native.Features.Reading;
using Reader.Presentation;

namespace Reader.Native.Services;

public static class AppState
{
    private static ReaderSession? session;
    private static string[] readingHistoryDocumentIds = [];
    internal static BackgroundTranslation? TranslationWorker { get; private set; }
    internal static int StorageRevision { get; private set; }
    internal static event Action? ReaderCacheInvalidated;
    internal static event Action? VoiceResourcesChanged;

    internal static void NotifyVoiceResourcesChanged() => VoiceResourcesChanged?.Invoke();

    internal static void InvalidateReaderCache()
    {
        StorageRevision++;
        ReaderCacheInvalidated?.Invoke();
    }

    internal static void StartTranslationWorker(IDispatcher dispatcher) =>
        TranslationWorker ??= new BackgroundTranslation(Session, dispatcher);
    // Bindings can still read the session while the native window is being destroyed.
    public static ReaderSession Session => session ??=
        new ReaderSession(Path.Combine(FileSystem.AppDataDirectory, "reader-native.db"));

    internal static void StopTranslationWorker()
    {
        TranslationWorker?.Stop();
        TranslationWorker = null;
    }

    internal static async Task RefreshRecentReadingHistoryAsync()
    {
        try
        {
            var revision = StorageRevision;
            var visits = await Session.RecentReadingSessionsAsync();
            if (revision == StorageRevision)
                readingHistoryDocumentIds = visits.Select(visit => visit.DocumentId).Distinct().ToArray();
        }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine($"Reading history: {error.Message}"); }
    }

    internal static IEnumerable<Document> RecentReadingDocuments(Snapshot snapshot)
    {
        var documents = snapshot.Documents.ToDictionary(document => document.Id, StringComparer.Ordinal);
        return Preferences.Get("recentDocuments", "").Split(';')
            .Prepend(Preferences.Get("lastDocument", ""))
            .Concat(readingHistoryDocumentIds)
            .Distinct(StringComparer.Ordinal)
            .Where(documents.ContainsKey)
            .Select(id => documents[id]);
    }

    internal static async Task ForgetRecentDocumentAsync(string id)
    {
        if (Preferences.Get("lastDocument", "") == id) Preferences.Remove("lastDocument");
        var remaining = Preferences.Get("recentDocuments", "").Split(';', StringSplitOptions.RemoveEmptyEntries)
            .Where(value => value != id);
        Preferences.Set("recentDocuments", string.Join(';', remaining));
        await RefreshRecentReadingHistoryAsync();
        (Shell.Current as AppShell)?.RefreshRecent();
    }

    public static async Task OpenAsync(string id, DictionarySourceLocation? source = null)
    {
        if (Shell.Current is not AppShell { IsClosing: false } shell || Session.Busy) return;
        if (source is null)
            await Session.SelectAsync(id);
        else
        {
            await Session.OpenDictionarySourceAsync(source);
            if (shell.IsClosing) return;
            if (Session.Error.Length > 0) throw new InvalidOperationException(Session.Error);
        }
        if (shell.IsClosing || Session.Error.Length > 0 || Session.Document?.Id != id) return;
        Preferences.Set("lastDocument", id);
        var recent = Preferences.Get("recentDocuments", "").Split(';').Prepend(id)
            .Where(value => value.Length != 0).Distinct().Take(3);
        Preferences.Set("recentDocuments", string.Join(";", recent));
        shell.RefreshRecent();
        await shell.GoToAsync(AppShell.ReadingRoute);
        if (!shell.IsClosing && source is not null && shell.CurrentPage is ReadingPage reader)
            await reader.RevealDictionarySourceAsync(source);
    }
}
