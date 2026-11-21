using System.ComponentModel;
using Reader.Contracts;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Presentation;

public sealed class ReaderSession : INotifyPropertyChanged, IDisposable
{
    private readonly Client client;
    private Snapshot snapshot = new([], []);
    private string? selectedId;
    private bool busy;
    private string error = "";
    public event PropertyChangedEventHandler? PropertyChanged;
    public Snapshot Snapshot => snapshot;
    public Document? Document => snapshot.Documents.FirstOrDefault(item => item.Id == selectedId);
    public Sentence? Sentence => Document?.Sentences.ElementAtOrDefault(Document.Position);
    public bool Busy => busy;
    public bool CanInteract => !busy;
    public string Error => error;
    public string Progress => Document is { } doc ? $"{doc.Position + 1} / {doc.Sentences.Length}" : "";
    public bool CanBookmark => !busy && Sentence is not null;
    public bool HasBookmarks => !busy && Document is { Bookmarks.Length: > 0 };
    public string BookmarkAction => Sentence is { } sentence && Document!.Bookmarks.Contains(sentence.Id)
        ? "Видалити закладку" : "Додати закладку";
    public string TranslationStatus => "Офлайн-переклад недоступний: мовний пакет не встановлено.";
    public ReaderSession(string path) => client = new Client(path);

    public Task RefreshAsync() => RunAsync(Read);

    public async Task SelectAsync(string id)
    {
        if (busy) { RejectBusy(); return; }
        selectedId = id;
        await RefreshAsync();
    }

    public Task ImportAsync(string name, byte[] bytes, string language) => RunAsync(() =>
    {
        var id = client.Import(name, bytes, language).Id;
        return (client.Snapshot(), id);
    });

    public Task MoveAsync(int delta) => SelectSentenceAsync((Document?.Position ?? 0) + delta);

    public Task SelectSentenceAsync(int position) => RunAsync(() =>
    {
        if (Document is not { } doc || position < 0 || position >= doc.Sentences.Length) return Read();
        client.Position(doc.Id, position);
        return Read();
    });

    public Task ToggleBookmarkAsync() => RunAsync(() =>
    {
        if (Document is { } document && Sentence is { } sentence)
            client.SetBookmark(document.Id, sentence.Id, !document.Bookmarks.Contains(sentence.Id));
        return Read();
    });

    public Task SaveWordAsync(TextSelection selection, string translation) => RunAsync(() =>
    {
        if (Document is not { } doc) return Read();
        client.SaveWord(new Vocabulary("", selection.Text, translation, doc.Language, "uk",
            Sentence?.Text ?? "", doc.Id, new ReviewState(), new ReviewState()));
        return Read();
    });

    public Task ReviewAsync(string id, string direction, byte grade) => RunAsync(() =>
    {
        client.Review(id, direction, grade);
        return Read();
    });

    public Task<Backup> ExportAsync() => Task.Run(client.Export);
    public Task<Original> OriginalAsync(string id) => Task.Run(() => client.Original(id));
    public Task RestoreAsync(Backup backup) => RunAsync(() =>
    {
        client.Restore(backup);
        return Read();
    });

    private (Snapshot Data, string? Selected) Read() => (client.Snapshot(), selectedId);

    private async Task RunAsync(Func<(Snapshot Data, string? Selected)> operation)
    {
        if (busy) { RejectBusy(); return; }
        busy = true;
        error = "";
        Notify();
        try
        {
            var result = await Task.Run(operation);
            snapshot = result.Data;
            selectedId = result.Selected;
        }
        catch (Exception exception) { error = exception.Message; }
        finally { busy = false; Notify(); }
    }

    private void Notify()
    {
        foreach (var name in new[] { nameof(Snapshot), nameof(Document), nameof(Sentence), nameof(Busy),
            nameof(CanInteract), nameof(Error), nameof(Progress), nameof(CanBookmark), nameof(HasBookmarks),
            nameof(BookmarkAction) })
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
    }

    private void RejectBusy()
    {
        error = "Попередня операція ще виконується. Зачекайте на її завершення.";
        Notify();
    }

    public void Dispose() => client.Dispose();
}
