using System.ComponentModel;
using Reader.Contracts;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Presentation;

public sealed class ReaderSession : INotifyPropertyChanged, IDisposable, IModelTransferPort
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
    public ReaderPreferences GetReaderPreferences() => client.ReaderPreferences();
    public ReaderPreferences MergeReaderPreferences(ReaderPreferences preferences) =>
        client.MergeReaderPreferences(preferences);
    public ReaderPreferences UpdateReaderPreference(ReaderPreferenceChange change) =>
        client.UpdateReaderPreference(change);
    public Task<ModelTransferPlan> ModelTransferStatusAsync(ModelTransferPackage package) =>
        Task.Run(() => client.ModelTransfer(package, "status"));

    public Task<ModelTransferPlan> ResumeModelTransferAsync(ModelTransferPackage package) =>
        Task.Run(() => client.ModelTransfer(package, "resume"));

    public Task<ModelTransferPlan> AcceptModelRangeAsync(
        ModelTransferPackage package, ModelTransferRange range, int status, string path) =>
        Task.Run(() => client.AcceptModelRange(package, range, status, path));

    public Task<ModelTransferPlan> AdoptModelPrefixAsync(ModelTransferPackage package, string asset, string path) =>
        Task.Run(() => client.AdoptModelPrefix(package, asset, path));

    public Task<ModelTransferPlan> PauseModelTransferAsync(ModelTransferPackage package) =>
        Task.Run(() => client.ModelTransfer(package, "pause"));

    public Task<ModelTransferPlan> ActivateModelTransferAsync(ModelTransferPackage package) =>
        Task.Run(() => client.ModelTransfer(package, "activate"));

    public Task<ModelTransferPlan> DiscardModelTransferAsync(ModelTransferPackage package) =>
        Task.Run(() => client.ModelTransfer(package, "discard"));
    public Task<ReadingSession[]> RecentReadingSessionsAsync() => Task.Run(client.RecentReadingSessions);
    public Task CheckpointReadingAsync(ReadingSession session) =>
        Task.Run(() => client.CheckpointReading(session));

    public Task<string?> DetectLanguageAsync(string text) => Task.Run(() => client.DetectLanguage(text));
    public Task<string?> DetectDocumentLanguageAsync(string name, byte[] bytes) =>
        Task.Run(() => client.DetectDocumentLanguage(name, bytes));

    public async Task SelectAsync(string id)
    {
        if (busy) { RejectBusy(); return; }
        selectedId = id;
        await RefreshAsync();
    }

    public Task ImportAsync(string name, byte[] bytes, string language, string? sourceUrl = null) => RunAsync(() =>
    {
        var id = client.Import(name, bytes, language, sourceUrl).Id;
        return (client.Snapshot(), id);
    });

    public bool CanViewOriginal => !busy && (Document?.Format == "pdf" || Document?.OcrLines is { Length: > 0 });

    public int OriginalPageIndex()
    {
        if (Document is not { } document || Sentence is not { } sentence) return 0;
        var cursorOffset = sentence.StartUtf16 + (document.ReadingTokenUtf16 ?? 0);
        if (document.PdfPageStarts is { } starts)
        {
            for (var index = starts.Length - 1; index >= 0; index--)
                if (starts[index] is int offsetOnPage && offsetOnPage <= cursorOffset) return index;
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
            if (cursorOffset < offset) return page;
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

    public T PdfOcrJob<T>(object job) => client.PdfOcrJob<T>(job);

    public Task ImportMixedPdfAsync(string name, byte[] bytes, string language, OcrLine[] lines, int pages) =>
        RunAsync(() =>
        {
            var id = client.ImportMixedPdf(name, bytes, language, lines, pages).Id;
            return (client.Snapshot(), id);
        });

    public Task ImportPdfFileAsync(string name, string path, string language, string[] pageTexts, OcrLine[] lines,
        int[]? emptyPages = null, string? sourceHash = null) =>
        RunAsync(() =>
        {
            var id = client.ImportPdfFile(name, path, language, pageTexts, lines, emptyPages, sourceHash).Id;
            return (client.Snapshot(), id);
        });

    public Task<IntakeItem> StagePdfIntakeAsync(Guid requestId, string name, string path) =>
        Task.Run(() => client.StagePdfIntake(requestId, name, path));

    public Task<IntakeItem> StageFileIntakeAsync(Guid requestId, string name, byte[] bytes) =>
        Task.Run(() => client.StageFileIntake(requestId, name, bytes));

    public Task<IntakeItem> StageTextIntakeAsync(Guid requestId, string name, string text) =>
        Task.Run(() => client.StageTextIntake(requestId, name, text));

    public Task<IntakeItem> StageLinkIntakeAsync(Guid requestId, string url) =>
        Task.Run(() => client.StageLinkIntake(requestId, url));

    public Task<IntakeItem[]> ListIntakeAsync() => Task.Run(() =>
    {
        var entries = new List<IntakeItem>();
        while (true)
        {
            var page = client.ListIntake(entries.Count);
            entries.AddRange(page);
            if (page.Length < 100) return entries.ToArray();
        }
    });

    public Task<IntakeItem?> FindIntakeAsync(Guid id) => Task.Run(() => client.FindIntake(id));

    public Task<IntakeFile> IntakeFileAsync(string id) => Task.Run(() => client.IntakeFile(id));

    public Task<byte[]> ReadIntakeBytesAsync(string id) => Task.Run(() => client.ReadIntakeBytes(id));

    public Task<string> ReadIntakeLinkAsync(string id) => Task.Run(() => client.ReadIntakeLink(id));

    public Task CompletePdfIntakeAsync(string id, string language, string[] pageTexts, int? deferredFrom = null) =>
        CompleteIntakeAsync(() => client.CompletePdfIntake(id, language, pageTexts, deferredFrom));

    public Task CompleteDocumentIntakeAsync(string id, string language) =>
        CompleteIntakeAsync(() => client.CompleteDocumentIntake(id, language));

    public Task CompletePhotoIntakeAsync(string id, string language, OcrLine[] lines) =>
        CompleteIntakeAsync(() => client.CompletePhotoIntake(id, language, lines));

    public Task CompleteArticleIntakeAsync(string id, string name, string text, string language, string sourceUrl) =>
        CompleteIntakeAsync(() => client.CompleteArticleIntake(id, name, text, language, sourceUrl));

    private Task CompleteIntakeAsync(Func<IntakeItem> complete) => RunAsync(() =>
    {
        var result = complete();
        return (client.Snapshot(), result.DocumentId ?? throw new InvalidDataException("Імпорт не підтверджено."));
    });

    public Task DiscardIntakeAsync(Guid id) => Task.Run(() => client.DiscardIntake(id));

    public Task<int?> PdfReadingPageAsync(string id) => Task.Run(() => client.PdfReadingPage(id));

    public Task SetPdfReadingPageAsync(string id, int page) => Task.Run(() => client.SetPdfReadingPage(id, page));

    public async Task<bool> OpenPdfReadingPageAsync(string id, string? revisionId, int page)
    {
        var hasText = false;
        await RunAsync(() =>
        {
            hasText = client.OpenPdfReadingPage(id, revisionId, page).HasValue;
            return Read();
        });
        return hasText && Error.Length == 0;
    }

    public Task<PdfReadWindow> PdfReadWindowAsync(string id, int center) =>
        Task.Run(() => client.PdfReadWindow(id, center));

    public Task<DetachedPdfSentence?> DetachedPdfSentenceAsync(string id, int page, int? position, string? sentenceId = null) =>
        Task.Run(() => client.DetachedPdfSentence(id, page, position, sentenceId));

    public Task<DetachedPdfFindPage> FindDetachedPdfTextAsync(
        string id, string query, DetachedPdfFindCursor start, int limit, CancellationToken cancellation) =>
        Task.Run(() => client.FindDetachedPdfText(id, query, start, limit), cancellation);

    public Task<DetachedPdfAnnotationItem[]> DetachedPdfAnnotationsAsync(string id, int page) =>
        Task.Run(() => client.DetachedPdfAnnotations(id, page));

    public Task<DetachedPdfSentence> UpdateDetachedPdfAnnotationAsync(string id, int page, string sentenceId, object change) =>
        Task.Run(() => client.UpdateDetachedPdfAnnotation(id, page, sentenceId, change));

    public Task<MachineTranslation?> TranslateDetachedPdfSentenceAsync(string id, int page, string sentenceId, string target) =>
        Task.Run(() => client.TranslateDetachedPdfSentence(id, page, sentenceId, target));

    public Task<PdfOcrRequest?> ClaimPdfOcrPageAsync(string id) => Task.Run(() => client.ClaimPdfOcrPage(id));

    public Task CachePdfOcrPageAsync(string id, PdfOcrRequest claim, OcrLine[] lines, string? textLayer = null) =>
        Task.Run(() => client.CachePdfOcrPage(id, claim, lines, textLayer));

    public Task ReleasePdfOcrPageAsync(string id, PdfOcrRequest claim, bool canceled) =>
        Task.Run(() => client.ReleasePdfOcrPage(id, claim, canceled));

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

    public Task SetDocumentLanguageAsync(string id, string language) => RunAsync(() =>
    {
        client.SetDocumentLanguage(id, language);
        return Read();
    });

    public Task DeleteDocumentAsync(string id) => RunAsync(() =>
    {
        client.DeleteDocument(id);
        return (client.Snapshot(), selectedId == id ? null : selectedId);
    });

    public Task RenameDocumentAsync(string id, string title) => RunAsync(() =>
    {
        client.RenameDocument(id, title);
        return Read();
    });

    public SpeechResume? GetSpeechResume(string id) => client.SpeechResume(id);

    public void SetSpeechResume(string id, string sentenceId, string textHash, int? offsetUtf16) =>
        client.SetSpeechResume(id, sentenceId, textHash, offsetUtf16);

    public void SetModelSpeechResume(string id, string sentenceId, string textHash, ModelSpeechResume model) =>
        client.SetModelSpeechResume(id, sentenceId, textHash, model);

    public Task SelectSentenceAsync(int position) => RunAsync(() =>
    {
        var documents = snapshot.Documents;
        var index = Array.FindIndex(documents, document => document.Id == selectedId);
        if (index < 0) return (snapshot, selectedId);

        var doc = documents[index];
        if (position < 0 || position >= doc.Sentences.Length || position == doc.Position)
            return (snapshot, selectedId);

        if (doc.CurrentRevisionId is { } revision)
            client.SetReadingCursor(doc.Id, doc.Sentences[position].Id, revision);
        else
            client.Position(doc.Id, position);
        var updated = (Document[])documents.Clone();
        updated[index] = doc with { Position = position, ReadingTokenUtf16 = null };
        return (snapshot with { Documents = updated }, selectedId);
    });

    public Task<ReadingCursor?> ReadingCursorAsync(string id) => Task.Run(() => client.ReadingCursor(id));

    public Task<WordLookup?> LookupSentenceTokenAsync(string id, string sentenceId, string revisionId, int startUtf16) =>
        Task.Run(() => client.LookupSentenceToken(id, sentenceId, revisionId, startUtf16));

    public Task<LinguisticAnalysis> AnalyzeSentenceAsync(string id, string sentenceId, string revisionId, int start) =>
        Task.Run(() => client.AnalyzeSentence(id, sentenceId, revisionId, start));

    public Task PrefetchLinguisticsAsync(
        string id, string sentenceId, string revisionId, int step, CancellationToken cancellation) =>
        Task.Run(() => client.PrefetchLinguistics(id, sentenceId, revisionId, step), cancellation);

    public Task<LinguisticAnalysis> AnalyzeDetachedPdfSentenceAsync(string id, int page, string sentenceId, int start) =>
        Task.Run(() => client.AnalyzeDetachedPdfSentence(id, page, sentenceId, start));

    public Task<LinguisticAnalysis> AnalyzeDetachedPdfSelectionAsync(string id, int page, TextSelection selection) =>
        Task.Run(() => client.AnalyzeDetachedPdfSelection(id, page, selection.StartUtf16, selection.Text));

    public Task<LinguisticAnalysis> AnalyzeExpressionAsync(
        string id, string sentenceId, string revisionId, int[] memberStartsUtf16) =>
        Task.Run(() => client.AnalyzeExpression(id, sentenceId, revisionId, memberStartsUtf16));

    public Task<int[]> ReadExpressionMembersAsync(string id, string sentenceId, string revisionId, TextSelection selection) =>
        Task.Run(() =>
        {
            var end = selection.StartUtf16 + selection.LengthUtf16;
            var members = new List<int>(8);
            int? start = 0;
            while (start is int pageStart)
            {
                var page = client.SentenceTokens(id, sentenceId, pageStart);
                if (page.RevisionId != revisionId) throw new InvalidOperationException("Текст документа змінився.");
                foreach (var token in page.Tokens)
                {
                    if (token.Span.StartUtf16 >= end) return members.ToArray();
                    if (token.Span.EndUtf16 <= selection.StartUtf16) continue;
                    members.Add(token.SentenceStartUtf16);
                    if (members.Count > 8) return members.ToArray();
                }
                start = page.NextStart;
            }
            return members.ToArray();
        });

    public async Task<ReadingCursor?> AnchorReadingSelectionAsync(string id, string revisionId, TextSelection selection)
    {
        ReadingCursor? anchored = null;
        await RunAsync(() =>
        {
            var documents = snapshot.Documents;
            var index = Array.FindIndex(documents, document => document.Id == id);
            if (index < 0 || id != selectedId || documents[index].CurrentRevisionId != revisionId)
                return (snapshot, selectedId);
            anchored = client.AnchorReadingSelection(id, revisionId, selection.StartUtf16, selection.LengthUtf16);
            if (anchored is not { } cursor) return (snapshot, selectedId);
            var updated = (Document[])documents.Clone();
            updated[index] = documents[index] with { Position = cursor.Position, ReadingTokenUtf16 = cursor.SentenceStartUtf16 };
            return (snapshot with { Documents = updated }, selectedId);
        });
        return Error.Length == 0 ? anchored : null;
    }
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

    public Task SetSentenceNoteAsync(string documentId, string sentenceId, string note) => RunAsync(() =>
    {
        client.SetSentenceNote(documentId, sentenceId, note);
        return Read();
    });

    public Task SetSpeechTextAsync(string documentId, string sentenceId, string text) => RunAsync(() =>
    {
        client.SetSpeechText(documentId, sentenceId, text);
        return Read();
    });

    public Task<WordExplanation> ExplainWordAsync(string model, string document, string sentence, string word) =>
        Task.Run(() => client.ExplainWord(model, document, sentence, word));

    public Task<string[]> SemanticVocabularyAsync(string query, string? documentId = null) =>
        Task.Run(() => client.SemanticVocabulary(query, documentId));

    public Task<OptionalModel[]> OptionalModelsAsync() => Task.Run(client.OptionalModels);
    public Task CheckOptionalModelSpaceAsync(string id, string stagingRoot) =>
        Task.Run(() => client.CheckOptionalModelSpace(id, stagingRoot));
    public Task InstallOptionalModelAsync(string id, string stagingRoot) =>
        Task.Run(() => client.InstallOptionalModel(id, stagingRoot));
    public Task ExportOptionalModelArchiveAsync(string id, string path) =>
        Task.Run(() => client.ExportOptionalModelArchive(id, path));
    public Task InstallOptionalModelArchiveAsync(string id, string path) =>
        Task.Run(() => client.InstallOptionalModelArchive(id, path));
    public Task RollbackOptionalModelAsync(string id) => Task.Run(() => client.RollbackOptionalModel(id));
    public Task VerifyOptionalModelAsync(string id) => Task.Run(() => client.VerifyOptionalModel(id));
    public Task<bool> RemoveOptionalModelAsync(string id) => Task.Run(() => client.RemoveOptionalModel(id));

    public Task<TranslationPackageStatus[]> TranslationPackagesAsync() => Task.Run(client.TranslationPackages);
    public Task<TranslationReadiness> TranslationReadinessAsync(string source, string target) =>
        Task.Run(() => client.GetTranslationReadiness(source, target));
    public Task<KnowledgePackage[]> KnowledgePackagesAsync() => Task.Run(client.KnowledgePackages);
    public Task<KnowledgePackage> InstallKnowledgePackageAsync(string language, string path) =>
        Task.Run(() => client.InstallKnowledgePackage(language, path));
    public Task<WordLookup?> LookupWordAsync(string language, string word, WordContext? context = null) =>
        Task.Run(() => client.LookupWord(language, word, context));
    public Task<OcrSuggestion[]?> SuggestOcrAsync(string language, string text) =>
        Task.Run(() => client.SuggestOcr(language, text));

    public Task ExportTranslationPackageAsync(string source, string target, string path) =>
        Task.Run(() => client.ExportTranslationPackage(source, target, path));
    public Task<string> InstallTranslationPackageAsync(string path) =>
        Task.Run(() => client.InstallTranslationPackage(path));
    public Task RollbackTranslationPackageAsync(string source, string target) =>
        Task.Run(() => client.RollbackTranslationPackage(source, target));
    public Task VerifyTranslationPackageAsync(string source, string target) =>
        Task.Run(() => client.VerifyTranslationPackage(source, target));
    public Task<TranslationPackageValidation> ValidateTranslationPackageAsync(string source, string target) =>
        Task.Run(() => client.ValidateTranslationPackage(source, target));
    public Task<bool> RemoveTranslationPackageAsync(string source, string target) =>
        Task.Run(() => client.RemoveTranslationPackage(source, target));

    public Task CheckTranslationDownloadSpaceAsync(string source, string target, string stagingRoot, string archiveDirectory) =>
        Task.Run(() => client.CheckTranslationDownloadSpace(source, target, stagingRoot, archiveDirectory));
    public Task CheckTranslationArchiveSpaceAsync(string directory, long bytes) =>
        Task.Run(() => client.CheckTranslationArchiveSpace(directory, bytes));

    public Task<ParagraphTranslation> TranslateParagraphAsync(string id, string sentenceId, string target, bool background = false) =>
        Task.Run(() => client.TranslateParagraph(id, sentenceId, target, background));
    public Task<CachedTranslation> ReadCachedTranslationAsync(string id, string sentenceId, string target) =>
        Task.Run(() => client.ReadCachedTranslation(id, sentenceId, target));
    public Task<TranslationFindPage> FindTranslationPageAsync(string id, string target, string query, int start,
        CancellationToken cancellationToken) =>
        Task.Run(() => client.FindTranslationPage(id, target, query, start), cancellationToken);
    public Task<LexicalPackage[]> LexicalPackagesAsync() => Task.Run(client.LexicalPackages);
    public Task<LexicalPackage> InstallLexicalPackageAsync(string language, string path) =>
        Task.Run(() => client.InstallLexicalPackage(language, path));
    public Task<DictionarySource[]> DictionarySourcesAsync() => Task.Run(client.DictionarySources);
    public Task<Vocabulary[]> DictionaryCollectionAsync(string? documentId = null) =>
        Task.Run(() => client.DictionaryCollection(documentId));
    public Task<DictionaryArchive> ExportDictionaryAsync(string? documentId = null) =>
        Task.Run(() => client.ExportDictionary(documentId));
    public async Task<DictionaryImport?> ImportDictionaryAsync(DictionaryArchive archive)
    {
        DictionaryImport? imported = null;
        await RunAsync(() =>
        {
            imported = client.ImportDictionary(archive);
            return Read();
        });
        return imported;
    }
    public Task<string> PrepareUkrainianSpeechAsync(string text) => Task.Run(() => client.PrepareUkrainianSpeech(text));
    public Task<SpeechAudio> SynthesizeSpeechAsync(string id, string sentenceId, uint speaker, float rate, string engine, int? page = null) =>
        Task.Run(() => client.SynthesizeSpeech(id, sentenceId, speaker, rate, engine, page));

    public Task PrefetchSpeechAsync(string id, string sentenceId, uint speaker, float rate, string engine,
        CancellationToken cancellation, int? page = null) =>
        Task.Run(() => client.PrefetchSpeech(id, sentenceId, speaker, rate, engine, page), cancellation);

    public Task<ParagraphTranslation?> PrefetchTranslationWindowAsync(
        string id, string sentenceId, string revisionId, string target) =>
        Task.Run(() =>
        {
            client.ScheduleTranslationWindow(id, sentenceId, revisionId, target);
            return client.RunTranslationWindow(id, sentenceId, revisionId, target);
        });

    public Task<MachineTranslation?> TranslateSentenceAsync(string id, string sentenceId, string target) =>
        Task.Run(() => client.TranslateSentence(id, sentenceId, target));

    public Task SaveManualVocabularyCardAsync(string word, string translation, string sourceLanguage,
        string targetLanguage) => RunAsync(() =>
    {
        client.SaveWord(new Vocabulary("", word, translation, sourceLanguage, targetLanguage, "", "",
            new ReviewState(), new ReviewState()));
        return Read();
    });

    public Task SaveWordAsync(TextSelection selection, string translation, LemmaSnapshot? analysis = null,
        ExpressionSnapshot? expression = null, SenseSnapshot? sense = null) => RunAsync(() =>
    {
        if (Document is not { } doc) return Read();
        var context = doc.Sentences.FirstOrDefault(sentence => selection.StartUtf16 >= sentence.StartUtf16
            && selection.StartUtf16 < sentence.EndUtf16)?.Text ?? "";
        client.SaveWord(new Vocabulary("", selection.Text, translation, doc.Language, "uk",
            context, doc.Id, new ReviewState(), new ReviewState()) { Analysis = analysis, Expression = expression, Sense = sense });
        return Read();
    });

    public Task SetVocabularyVariantsAsync(string id, string[] acceptedTranslations, string? translation = null) => RunAsync(() =>
    {
        client.SetVocabularyVariants(id, acceptedTranslations, translation);
        return Read();
    });

    public Task DeleteVocabularyCardAsync(string id) => RunAsync(() =>
    {
        client.DeleteVocabularyCard(id);
        return Read();
    });

    public Task<ReviewEvent[]> VocabularyReviewHistoryAsync(string id) =>
        Task.Run(() => client.VocabularyReviewHistory(id));

    public Task ReviewAsync(string id, string direction, byte grade) => RunAsync(() =>
    {
        client.Review(id, direction, grade);
        return Read();
    });

    public PracticeProgress GetPractice() => client.GetPractice();
    public PracticeProgress SetPractice(PracticeProgress progress) => client.SetPractice(progress);
    public Task<PracticeProgress> GetPracticeAsync() => Task.Run(client.GetPractice);
    public Task<PracticeQueue> NextPracticeQuestionAsync(ulong revision, PracticeSettings? settings = null) =>
        Task.Run(() => client.NextPracticeQuestion(revision, settings));

    public Task<PracticeAnswer> AnswerPracticeQuestionAsync(ulong revision, string? answer, uint elapsedMs) =>
        Task.Run(() => client.AnswerPracticeQuestion(revision, answer, elapsedMs));

    public Task GradePracticeAsync(ulong revision, byte grade) => RunAsync(() =>
    {
        client.GradePractice(revision, grade);
        return Read();
    });

    public Task<Backup> ExportAsync() => Task.Run(client.Export);
    public Task ExportBackupFileAsync(string path) => Task.Run(() => client.ExportBackupFile(path));
    public Task<BackupPreview> InspectBackupFileAsync(string path) => Task.Run(() => client.InspectBackupFile(path));
    public async Task<BackupResource[]?> RestoreBackupFileAsync(string path)
    {
        BackupResource[]? resources = null;
        await RunAsync(() =>
        {
            var restored = client.RestoreBackupFile(path);
            var state = Read();
            resources = restored;
            return state;
        });
        return resources;
    }

    public Task<Original> OriginalAsync(string id) => Task.Run(() => client.Original(id));
    public Task<OriginalFile?> OriginalFileAsync(string id) => Task.Run(() => client.OriginalFile(id));
    public Task RestoreAsync(Backup backup) => RunAsync(() =>
    {
        client.Restore(backup);
        return Read();
    });

    private (Snapshot Data, string? Selected) Read() => (client.Snapshot(), selectedId);

    public async Task WaitUntilIdleAsync()
    {
        if (!busy) return;
        var completed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        void OnChanged(object? sender, PropertyChangedEventArgs args)
        {
            if (args.PropertyName == nameof(Busy) && !busy) completed.TrySetResult();
        }
        PropertyChanged += OnChanged;
        try
        {
            if (busy) await completed.Task;
        }
        finally { PropertyChanged -= OnChanged; }
    }

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
