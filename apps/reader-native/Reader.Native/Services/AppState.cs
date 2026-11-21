using Reader.Presentation;

namespace Reader.Native.Services;

public static class AppState
{
    public static ReaderSession Session { get; } =
        new(Path.Combine(FileSystem.AppDataDirectory, "reader-native.db"));

    public static async Task OpenAsync(string id)
    {
        await Session.SelectAsync(id);
        Preferences.Set("lastDocument", id);
        await Shell.Current.GoToAsync("//reading");
    }
}
