using Reader.Presentation;

namespace Reader.Native.Services;

public static class AppState
{
    private static ReaderSession? session;
    public static ReaderSession Session => session ??=
        new ReaderSession(Path.Combine(FileSystem.AppDataDirectory, "reader-native.db"));

    public static void Close()
    {
        session?.Dispose();
        session = null;
    }

    public static async Task OpenAsync(string id)
    {
        if (Session.Busy) return;
        await Session.SelectAsync(id);
        if (Session.Error.Length > 0 || Session.Document?.Id != id) return;
        Preferences.Set("lastDocument", id);
        var recent = Preferences.Get("recentDocuments", "").Split(';').Prepend(id)
            .Where(value => value.Length != 0).Distinct().Take(3);
        Preferences.Set("recentDocuments", string.Join(";", recent));
        (Shell.Current as AppShell)?.RefreshRecent();
        await Shell.Current.GoToAsync("//reading");
    }
}
