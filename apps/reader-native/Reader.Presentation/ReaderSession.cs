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
    public string? ReviewedTranslation(string target) =>
        Document is { } document && Sentence is { } sentence &&
        document.ReviewedTranslations.TryGetValue(target, out var translations) &&
        translations.TryGetValue(sentence.Id, out var text) ? text : null;
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

    public bool CanViewOriginal => !busy && (Document?.Format == "pdf" || Document?.OcrLines is { Length: > 0 });

    public int OriginalPageIndex()
    {
        if (Document is not { } document || Sentence is not { } sentence) return 0;
        if (document.PdfPageStarts is { } starts)
        {
            for (var index = starts.Length - 1; index >= 0; index--)
                if (starts[index] is int offsetOnPage && offsetOnPage <= sentence.StartUtf16) return index;
            return 0;
        }
        if (document.OcrLines is not { Length: > 0 } lines) return 0;
        var offset = 0;
        var page = lines[0].Page;
        foreach (var line in lines)
        {
            if (offset != 0) offset += line.Page == page ? 1 : 2;
            offset += (line.Correction ?? line.Text).Trim().Length;
            page = line.Page;
            if (sentence.StartUtf16 < offset) return page;
        }
        return page;
    }

    public Task ImportImageAsync(string name, byte[] bytes, string language, OcrLine[] lines) => RunAsync(() =>
    {
        var id = client.ImportImage(name, bytes, language, lines).Id;
        return (client.Snapshot(), id);
    });

    public Task<int[]> PdfPagesNeedingOcrAsync(string name, byte[] bytes, int pages) =>
        Task.Run(() => client.PdfPagesNeedingOcr(name, bytes, pages));

    public Task ImportMixedPdfAsync(string name, byte[] bytes, string language, OcrLine[] lines, int pages) =>
        RunAsync(() =>
        {
            var id = client.ImportMixedPdf(name, bytes, language, lines, pages).Id;
            return (client.Snapshot(), id);
        });

    public Task ImportScannedPdfAsync(string name, byte[] bytes, string language, OcrLine[] lines, int pages) =>
        RunAsync(() =>
        {
            var id = client.ImportScannedPdf(name, bytes, language, lines, pages).Id;
            return (client.Snapshot(), id);
        });

    public Task CorrectOcrLineAsync(string id, int index, string? correction) => RunAsync(() =>
    {
        client.CorrectOcrLine(id, index, correction);
        return Read();
    });

    public Task MoveAsync(int delta) => SelectSentenceAsync((Document?.Position ?? 0) + delta);

    public SpeechResume? GetSpeechResume(string id) => client.SpeechResume(id);

    public void SetSpeechResume(string id, string sentenceId, string textHash, int? offsetUtf16) =>
        client.SetSpeechResume(id, sentenceId, textHash, offsetUtf16);

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

    public Task SetReviewedTranslationAsync(string documentId, string sentenceId, string target, string text) =>
        RunAsync(() =>
        {
            client.SetReviewedTranslation(documentId, sentenceId, target, text);
            return Read();
        });

    public Task<TranslationPackageStatus[]> TranslationPackagesAsync() => Task.Run(client.TranslationPackages);
    public Task<KnowledgePackage[]> KnowledgePackagesAsync() => Task.Run(client.KnowledgePackages);
    public Task<KnowledgePackage> InstallKnowledgePackageAsync(string language, string path) =>
        Task.Run(() => client.InstallKnowledgePackage(language, path));
    public Task<WordLookup?> LookupWordAsync(string language, string word) =>
        Task.Run(() => client.LookupWord(language, word));

    public Task<string> InstallTranslationPackageAsync(string path) =>
        Task.Run(() => client.InstallTranslationPackage(path));

    public Task<MachineTranslation?> TranslateSentenceAsync(string id, string sentenceId, string target) =>
        Task.Run(() => client.TranslateSentence(id, sentenceId, target));

    public Task SaveWordAsync(TextSelection selection, string translation, LemmaSnapshot? analysis = null) => RunAsync(() =>
    {
        if (Document is not { } doc) return Read();
        var context = doc.Sentences.FirstOrDefault(sentence => selection.StartUtf16 >= sentence.StartUtf16
            && selection.StartUtf16 < sentence.EndUtf16)?.Text ?? "";
        client.SaveWord(new Vocabulary("", selection.Text, translation, doc.Language, "uk",
            context, doc.Id, new ReviewState(), new ReviewState()) { Analysis = analysis });
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
            nameof(BookmarkAction), nameof(CanViewOriginal) })
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
    }

    private void RejectBusy()
    {
        error = "Попередня операція ще виконується. Зачекайте на її завершення.";
        Notify();
    }

    public void Dispose() => client.Dispose();
}
