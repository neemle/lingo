using System.Runtime.InteropServices;
using System.Text.Json;
using Reader.Contracts;

namespace Reader.CoreClient;

public sealed class CoreClient : IDisposable
{
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);
    private readonly object sync = new();
    private readonly StoreHandle handle;
    private bool disposed;
    private long nextModelTranslationRequest;
    private long nextSpeechRequest;
    private long nextSpeechPrefetchRequest;
    public const int MaxImportBytes = 20 * 1024 * 1024;
    public const long MaxPdfBytes = 4L * 1024 * 1024 * 1024;
    public const int MaxBackupFileBytes = 1024 * 1024 * 1024;

    public CoreClient(string databasePath)
    {
        if (NativeMethods.reader_abi_version() != 1) throw new NotSupportedException("Rust ABI v1 required");
        var opened = Invoke<OpenResult>(0, new { version = 1, operation = "open", path = databasePath });
        handle = new StoreHandle(opened.Handle);
    }

    public Snapshot Snapshot() => Call<Snapshot>(new { version = 1, operation = "snapshot" });
    public StorageUsage StorageUsage() =>
        Call<StorageUsage>(new { version = 1, operation = "storage_usage" });

    public void ClearStorage(string scope) =>
        CallIndependent<JsonElement>(new { version = 1, operation = "clear_storage", scope });

    public BackgroundTranslationProgress AdvanceBackgroundTranslation(string id, string revision, string target) =>
        CallIndependent<BackgroundTranslationProgress>(new { version = 1, operation = "advance_background_translation", id, revision, target });

    public TranslationWarmStep WarmTranslationStep(string id, string revision, string target, int start) =>
        CallIndependent<TranslationWarmStep>(new { version = 1, operation = "warm_translation_step", id, revision, target, start });

    public ReaderPreferences ReaderPreferences() =>
        Call<ReaderPreferences>(new { version = 1, operation = "reader_preferences" });

    public ReaderPreferences MergeReaderPreferences(ReaderPreferences preferences) =>
        Call<ReaderPreferences>(new { version = 1, operation = "merge_reader_preferences", preferences });

    public ReaderPreferences UpdateReaderPreference(ReaderPreferenceChange change) =>
        Call<ReaderPreferences>(new { version = 1, operation = "update_reader_preference", change });
    public ModelTransferPlan ModelTransfer(ModelTransferPackage package, string operation) =>
        ModelTransfer(package, new { kind = operation });

    public ModelTransferPlan AcceptModelRange(ModelTransferPackage package, ModelTransferRange range, int status, string path) =>
        ModelTransfer(package, new
        {
            kind = "acceptRange", asset = range.Asset, path, status, @from = range.From, to = range.To, total = range.Total,
        });

    public ModelTransferPlan AdoptModelPrefix(ModelTransferPackage package, string asset, string path) =>
        ModelTransfer(package, new { kind = "adoptPrefix", asset, path });

    private ModelTransferPlan ModelTransfer(ModelTransferPackage package, object action) =>
        CallIndependent<ModelTransferPlan>(new { version = 1, operation = "model_transfer", package, action });
    public ReadingSession[] RecentReadingSessions() =>
        Call<ReadingSession[]>(new { version = 1, operation = "recent_reading_sessions" });
    public void CheckpointReading(ReadingSession session) =>
        Call<JsonElement>(new { version = 1, operation = "checkpoint_reading", session });
    public string? DetectLanguage(string text)
    {
        var length = Math.Min(text.Length, 8000);
        if (length > 0 && char.IsHighSurrogate(text[length - 1])) length--;
        return Call<string?>(new { version = 1, operation = "detect_language", text = text[..length] });
    }
    public string? DetectDocumentLanguage(string name, byte[] bytes) =>
        Call<string?>(new { version = 1, operation = "detect_document_language", name,
            bytes = bytes.Select(value => (int)value).ToArray() });
    public Backup Export() => Call<Backup>(new { version = 1, operation = "export" });
    public void ExportBackupFile(string path) =>
        Call<JsonElement>(new { version = 1, operation = "export_backup_file", path });
    public BackupPreview InspectBackupFile(string path) =>
        CallIndependent<BackupPreview>(new { version = 1, operation = "inspect_backup_file", path });
    public BackupResource[] RestoreBackupFile(string path) =>
        Call<BackupResource[]>(new { version = 1, operation = "restore_backup_file", path });
    public Original Original(string id) => Call<Original>(new { version = 1, operation = "original", id });
    public OriginalFile? OriginalFile(string id) =>
        Call<OriginalFile?>(new { version = 1, operation = "original_file", id });

    public IntakeItem StagePdfIntake(Guid requestId, string name, string path) =>
        StageIntake(requestId, new { kind = "pdfFile", name, path });

    public IntakeItem StageFileIntake(Guid requestId, string name, byte[] bytes) =>
        StageIntake(requestId, new { kind = "file", name, bytes = Array.ConvertAll(bytes, value => (int)value) });

    public IntakeItem StageTextIntake(Guid requestId, string name, string text) =>
        StageIntake(requestId, new { kind = "text", name, text });

    public IntakeItem StageLinkIntake(Guid requestId, string url) =>
        StageIntake(requestId, new { kind = "link", url });

    private IntakeItem StageIntake(Guid requestId, object input) =>
        CallIndependent<IntakeItem>(new { version = 1, operation = "stage_intake", requestId, input });

    public IntakeItem[] ListIntake(int start) =>
        Call<IntakeItem[]>(new { version = 1, operation = "list_intake", start });

    public IntakeItem? FindIntake(Guid id) =>
        Call<IntakeItem?>(new { version = 1, operation = "find_intake", id });

    public IntakeFile IntakeFile(string id) =>
        Call<IntakeFile>(new { version = 1, operation = "intake_payload", id });

    public byte[] ReadIntakeBytes(string id) =>
        Array.ConvertAll(Call<IntakeBytes>(new { version = 1, operation = "intake_payload", id }).Bytes,
            value => checked((byte)value));

    public string ReadIntakeLink(string id) =>
        Call<IntakeLink>(new { version = 1, operation = "intake_payload", id }).Url;

    public IntakeArticleDraft? IntakeArticle(string id) =>
        Call<IntakeArticleDraft?>(new { version = 1, operation = "intake_article", id });

    public IntakeArticleDraft SaveIntakeArticle(string id, IntakeArticleDraft draft) =>
        Call<IntakeArticleDraft>(new { version = 1, operation = "save_intake_article", id, draft });

    public IntakeItem CompleteStagedArticleIntake(string id, string language) =>
        CompleteIntake(id, new { kind = "stagedArticle", language });

    public IntakeItem CompletePdfIntake(string id, string language, string[] pageTexts, int? deferredFrom = null) =>
        CompleteIntake(id, new { kind = "pdf", language, pageTexts, deferredFrom });

    public string? IntakePdfTextSample(string id) =>
        Call<string?>(new { version = 1, operation = "intake_pdf_text_sample", id });

    public string SaveIntakePdfText(string id, string[] pageTexts, int? deferredFrom) =>
        Call<string>(new { version = 1, operation = "save_intake_pdf_text", id, draft = new { pageTexts, deferredFrom } });

    public IntakeItem CompleteStagedPdfIntake(string id, string language) =>
        CompleteIntake(id, new { kind = "stagedPdf", language });

    public IntakeItem CompleteDocumentIntake(string id, string language) =>
        CompleteIntake(id, new { kind = "document", language });

    public IntakeItem CompletePhotoIntake(string id, string language, OcrLine[] lines) =>
        CompleteIntake(id, new { kind = "photo", language, lines });

    public string? IntakePhotoOcrLanguage(string id) =>
        Call<string?>(new { version = 1, operation = "intake_photo_ocr_language", id });

    public void SaveIntakePhotoOcr(string id, string language, OcrLine[] lines) =>
        Call<JsonElement>(new { version = 1, operation = "save_intake_photo_ocr", id, language, lines });

    public IntakeItem CompleteStagedPhotoIntake(string id, string language) =>
        CompleteIntake(id, new { kind = "stagedPhoto", language });

    public IntakeItem CompleteArticleIntake(string id, string name, string text, string language, string sourceUrl) =>
        CompleteIntake(id, new { kind = "article", name, text, language, sourceUrl });

    private IntakeItem CompleteIntake(string id, object result) =>
        CallIndependent<IntakeItem>(new { version = 1, operation = "complete_intake", id, result });

    public bool DiscardIntake(Guid id) =>
        CallIndependent<bool>(new { version = 1, operation = "discard_intake", id });

    public int? PdfReadingPage(string id) =>
        Call<int?>(new { version = 1, operation = "pdf_reading_page", id });

    public void SetPdfReadingPage(string id, int page) =>
        Call<JsonElement>(new { version = 1, operation = "set_pdf_reading_page", id, page });

    public int? OpenPdfReadingPage(string id, string? revisionId, int page) =>
        Call<int?>(new { version = 1, operation = "open_pdf_reading_page", id, revisionId, page });

    public PdfReadWindow PdfReadWindow(string id, int center) =>
        Call<PdfReadWindow>(new { version = 1, operation = "pdf_read_window", id, center });
    public int[] UnreadPdfTextPages(string id) =>
        Call<int[]>(new { version = 1, operation = "unread_pdf_text_pages", id });

    public DetachedPdfSentence? DetachedPdfSentence(string id, int page, int? position, string? sentenceId = null, int? sourceOffset = null) =>
        Call<DetachedPdfSentence?>(new { version = 1, operation = "detached_pdf_sentence", id, page, position, sentenceId, sourceOffset });

    public DetachedPdfFindPage FindDetachedPdfText(string id, string query, DetachedPdfFindCursor start, int limit, string? target = null) =>
        CallIndependent<DetachedPdfFindPage>(new { version = 1, operation = "find_detached_pdf_text", id, query, start, limit, target });

    public void SetDetachedPdfSpeechResume(string id, int page, string sentenceId, SpeechResume? resume) =>
        Call<JsonElement>(new { version = 1, operation = "set_detached_pdf_speech_resume", id, page, sentenceId, resume });

    public DetachedPdfOcrPage? DetachedPdfOcr(string id, int page) =>
        Call<DetachedPdfOcrPage?>(new { version = 1, operation = "detached_pdf_ocr", id, page });

    public void FocusDetachedPdfOcrWord(string id, int page, string revision, int startUtf16, string word) =>
        Call<JsonElement>(new { version = 1, operation = "focus_detached_pdf_ocr_word", id, page, revision, startUtf16, word });

    public DetachedPdfOcrPage CorrectDetachedPdfOcr(string id, int page, string revision, int index, string? correction) =>
        Call<DetachedPdfOcrPage>(new { version = 1, operation = "correct_detached_pdf_ocr", id, page, revision, index, correction });

    public DetachedPdfAnnotationItem[] DetachedPdfAnnotations(string id, int page) =>
        Call<DetachedPdfAnnotationItem[]>(new { version = 1, operation = "detached_pdf_annotations", id, page });

    public DetachedPdfSentence UpdateDetachedPdfAnnotation(string id, int page, string sentenceId, object change) =>
        Call<DetachedPdfSentence>(new
        {
            version = 1, operation = "update_detached_pdf_annotation", id, page, sentenceId, change,
        });

    public DetachedPdfTranslation? GetDetachedPdfTranslation(string id, int page, string sentenceId, string target,
        bool cachedOnly, bool background) =>
        CallIndependent<DetachedPdfTranslation?>(new
        {
            version = 1, operation = "detached_pdf_translation", id, page, sentenceId, target, cachedOnly, background,
        });

    public DetachedPdfPassage GetDetachedPdfPassage(string id, int page, string sentenceId, string target,
        bool cachedOnly, bool background) =>
        CallIndependent<DetachedPdfPassage>(new
        {
            version = 1, operation = "detached_pdf_passage", id, page, sentenceId, target, cachedOnly, background,
        });

    public MachineTranslation? TranslateDetachedPdfSentence(string id, int page, string sentenceId, string target) =>
        CallIndependent<MachineTranslation?>(new
        {
            version = 1, operation = "translate_detached_pdf_sentence", id, page, sentenceId, target,
        });

    public PdfOcrRequest? ClaimPdfOcrPage(string id, int? center = null) =>
        Call<PdfOcrRequest?>(new { version = 1, operation = "claim_pdf_ocr_page", id, center });

    public bool CacheBackgroundPdfTextLayer(string id, string revision, int page, string text) =>
        Call<bool>(new { version = 1, operation = "cache_background_pdf_text_layer", id, revision, page, text });

    public PdfPageTranslationRow[] PdfPageTranslation(string id, int page, string target, bool cachedOnly, bool background = false) =>
        CallIndependent<PdfPageTranslationRow[]>(new
        {
            version = 1, operation = "pdf_page_translation", id, page, target, cachedOnly, background,
        });

    public void CachePdfOcrPage(string id, PdfOcrRequest claim, OcrLine[] lines, string? textLayer = null) =>
        Call<JsonElement>(new
        {
            version = 1, operation = "cache_pdf_ocr_page", id, page = claim.Page, token = claim.Token, lines, textLayer,
        });

    public void ReplacePdfTextLayer(string id, int page, string revisionId, string language, OcrLine[] lines) =>
        Call<JsonElement>(new { version = 1, operation = "replace_pdf_text_layer", id, page, revisionId, language, lines });

    public bool ReconcilePdfOcrPage(string id, int page, string revisionId, string language, string reference, bool detached) =>
        Call<bool>(new { version = 1, operation = "reconcile_pdf_ocr_page", id, page, revisionId, language, reference, detached });

    public bool RepairStoredPdfTextOrder(string id, int page, string revisionId, string reference, PdfTextOrderEdit[] changes) =>
        Call<bool>(new { version = 1, operation = "repair_stored_pdf_text_order", id, page, revisionId, reference, changes });

    public bool ReconcilePhotoOcr(string id, string revisionId) =>
        Call<bool>(new { version = 1, operation = "reconcile_photo_ocr", id, revisionId });

    public OcrWordReview? ReviewOcrWord(string id, int? page, string revisionId, int startUtf16, string word, bool cachedOnly) =>
        CallIndependent<OcrWordReview?>(new
        {
            version = 1, operation = "review_ocr_word", id, page, revisionId, startUtf16, word, cachedOnly,
        });

    public void ReleasePdfOcrPage(string id, PdfOcrRequest claim, bool canceled) =>
        Call<bool>(new
        {
            version = 1, operation = "retry_pdf_ocr_page", id, page = claim.Page, token = claim.Token, canceled,
        });

    public Document ImportPdfFile(string name, string path, string language, string[] pageTexts, OcrLine[] lines,
        int[]? emptyPages = null, string? sourceHash = null) =>
        CallIndependent<Document>(new
        {
            version = 1, operation = "import_pdf_file", name, path, language, pageTexts, lines,
            emptyPages = emptyPages ?? [], sourceHash,
        });

    public Document Import(string name, ReadOnlySpan<byte> bytes, string language, string? sourceUrl = null)
    {
        if (bytes.Length is 0 or > MaxImportBytes)
            throw new ArgumentOutOfRangeException(nameof(bytes), "Import requires 1..20MiB");
        var payload = bytes.ToArray().Select(value => (int)value).ToArray();
        return sourceUrl is null
            ? Call<Document>(new { version = 1, operation = "import", name, bytes = payload, language })
            : Call<Document>(new { version = 1, operation = "import", name, bytes = payload, language, sourceUrl });
    }
    public Document ImportImage(string name, byte[] bytes, string language, OcrLine[] lines)
    {
        if (bytes.Length is 0 or > MaxImportBytes)
            throw new ArgumentOutOfRangeException(nameof(bytes), "Import requires 1..20MiB");
        return Call<Document>(new { version = 1, operation = "import_image", name,
            bytes = bytes.Select(value => (int)value).ToArray(), language, lines });
    }

    public int[] PdfPagesNeedingOcr(string name, byte[] bytes, int pages) =>
        Call<int[]>(new { version = 1, operation = "pdf_pages_needing_ocr", name,
            bytes = bytes.Select(value => (int)value).ToArray(), pages });

    public T PdfOcrJob<T>(object job) => Call<T>(new { version = 1, operation = "pdf_ocr_job", job });

    public Document ImportMixedPdf(string name, byte[] bytes, string language, OcrLine[] lines, int pages) =>
        Call<Document>(new { version = 1, operation = "import_mixed_pdf", name,
            bytes = bytes.Select(value => (int)value).ToArray(), language, lines, pages });

    public Document ImportScannedPdf(string name, byte[] bytes, string language, OcrLine[] lines, int pages)
    {
        if (bytes.Length is 0 or > MaxImportBytes)
            throw new ArgumentOutOfRangeException(nameof(bytes), "Import requires 1..20MiB");
        return Call<Document>(new { version = 1, operation = "import_scanned_pdf", name,
            bytes = bytes.Select(value => (int)value).ToArray(), language, lines, pages });
    }

    public void CorrectOcrLine(string id, int index, string? correction) =>
        Call<JsonElement>(new { version = 1, operation = "correct_ocr_line", id, index, correction });

    public void Position(string id, int position) =>
        Call<JsonElement>(new { version = 1, operation = "position", id, position });

    public ReadingCursor? ReadingCursor(string id) =>
        Call<ReadingCursor?>(new { version = 1, operation = "reading_cursor", id });

    public ReadingCursor SetReadingCursor(string id, string sentenceId, string revisionId) =>
        Call<ReadingCursor>(new { version = 1, operation = "set_reading_cursor", id, sentenceId, revisionId });

    public ReadingCursor? AnchorReadingSelection(string id, string revisionId, int startUtf16, int lengthUtf16) =>
        Call<ReadingCursor?>(new
        {
            version = 1, operation = "anchor_reading_selection", id, revisionId, startUtf16, lengthUtf16,
        });

    public WordLookup? LookupSentenceToken(string id, string sentenceId, string revisionId, int startUtf16) =>
        Call<WordLookup?>(new { version = 1, operation = "lookup_sentence_token", id, sentenceId, revisionId, startUtf16 });

    public SentenceTokenPage SentenceTokens(string id, string sentenceId, int start) =>
        Call<SentenceTokenPage>(new { version = 1, operation = "sentence_tokens", id, sentenceId, start });

    public LinguisticAnalysis AnalyzeWord(string id, string sentenceId, string revisionId, int startUtf16) =>
        AnalyzeLinguistics(id, sentenceId, revisionId, new { kind = "word", startUtf16 });

    public LinguisticAnalysis AnalyzeSentence(string id, string sentenceId, string revisionId, int start) =>
        AnalyzeLinguistics(id, sentenceId, revisionId, new { kind = "sentence", start });

    public void PrefetchLinguistics(string id, string sentenceId, string revisionId, int step, int? page = null) =>
        CallIndependent<JsonElement>(new
        {
            version = 1, operation = "prefetch_linguistics", id, sentenceId, revisionId, step, page,
        });

    public LinguisticAnalysis AnalyzeDetachedPdfSentence(string id, int page, string sentenceId, int start) =>
        AnalyzeDetachedPdf(id, page, new { kind = "sentence", sentenceId, start });

    public LinguisticAnalysis AnalyzeDetachedPdfSelection(string id, int page, int startUtf16, string text) =>
        AnalyzeDetachedPdf(id, page, new { kind = "selection", startUtf16, text });

    public LinguisticAnalysis AnalyzeDetachedPdfExpression(string id, int page, string sentenceId,
        string revisionId, IReadOnlyList<int> memberStartsUtf16) =>
        AnalyzeDetachedPdf(id, page, new { kind = "expression", sentenceId, revisionId, memberStartsUtf16 });

    private LinguisticAnalysis AnalyzeDetachedPdf(string id, int page, object request) =>
        CallIndependent<LinguisticAnalysis>(new { version = 1, operation = "analyze_detached_pdf", id, page, request });

    public LinguisticAnalysis AnalyzeExpression(string id, string sentenceId, string revisionId, int[] memberStartsUtf16) =>
        AnalyzeLinguistics(id, sentenceId, revisionId, new { kind = "expression", memberStartsUtf16 });

    private LinguisticAnalysis AnalyzeLinguistics(string id, string sentenceId, string revisionId, object scope) =>
        CallIndependent<LinguisticAnalysis>(new { version = 1, operation = "analyze_linguistics", id, sentenceId, revisionId, scope });
    public void SetDocumentLanguage(string id, string language) =>
        Call<JsonElement>(new { version = 1, operation = "set_document_language", id, language });
    public bool DeleteDocument(string id) =>
        Call<bool>(new { version = 1, operation = "delete_document", id });
    public void RenameDocument(string id, string title) =>
        Call<JsonElement>(new { version = 1, operation = "rename_document", id, title });
    public string? SpeechNarration(string id, string sentenceId, bool pronunciationOnly = false) =>
        Call<string?>(new { version = 1, operation = "speech_narration", id, sentenceId, pronunciationOnly });
    public SpeechResume? SpeechResume(string id) =>
        Call<SpeechResume?>(new { version = 1, operation = "speech_resume", id });
    public void SetSpeechResume(string id, string sentenceId, string textHash, int? offsetUtf16) =>
        Call<JsonElement>(new { version = 1, operation = "set_speech_resume", id, sentenceId, textHash, offsetUtf16 });
    public void SetModelSpeechResume(string id, string sentenceId, string textHash, ModelSpeechResume model) =>
        Call<JsonElement>(new { version = 1, operation = "set_speech_resume", id, sentenceId, textHash, model });
    public void SetBookmark(string id, string sentenceId, bool saved) =>
        Call<JsonElement>(new { version = 1, operation = "set_bookmark", id, sentenceId, saved });
    public void SetSentenceNote(string id, string sentenceId, string note) =>
        Call<JsonElement>(new { version = 1, operation = "set_sentence_note", id, sentenceId, note });
    public void SetSpeechText(string id, string sentenceId, string text) =>
        Call<JsonElement>(new { version = 1, operation = "set_speech_text", id, sentenceId, text });
    public void SetReviewedTranslation(string id, string sentenceId, string target, string text) =>
        Call<JsonElement>(new { version = 1, operation = "set_reviewed_translation", id, sentenceId, target, text });
    public SentenceReview? ReviewSentence(string model, string document, int? page, string? sentence,
        object? focus = null, string? revisionId = null, bool cachedOnly = false, bool explainSyntax = false) =>
        CallIndependent<SentenceReview?>(new
        {
            version = 1, operation = "review_sentence", model, document, page, sentence, focus, revisionId, cachedOnly, explainSyntax,
        });

    public WordExplanation? ExplainWord(string model, string document, string sentence, string word,
        bool cachedOnly = false, int? startUtf16 = null, string? revisionId = null) =>
        CallIndependent<WordExplanation?>(new
        {
            version = 1, operation = "explain_word", model, document, sentence, word,
            startUtf16, revisionId, cachedOnly
        });

    public WordExplanation? ExplainDetachedWord(string model, string document, int page, int startUtf16,
        string word, string? revisionId = null, bool cachedOnly = false) =>
        CallIndependent<WordExplanation?>(new
        {
            version = 1, operation = "explain_detached_word", model, document, page,
            startUtf16, word, revisionId, cachedOnly
        });
    public string[] SemanticVocabulary(string query, string? documentId = null) =>
        CallIndependent<string[]>(new { version = 1, operation = "semantic_vocabulary", query, documentId });

    public OptionalModel[] OptionalModels() =>
        Call<OptionalModel[]>(new { version = 1, operation = "optional_models" });
    public void CheckOptionalModelSpace(string id, string stagingRoot) =>
        Call<JsonElement>(new { version = 1, operation = "check_optional_model_space", id, stagingRoot });
    public void InstallOptionalModel(string id, string stagingRoot) =>
        CallIndependent<JsonElement>(new { version = 1, operation = "install_optional_model", id, stagingRoot });
    public void ExportOptionalModelArchive(string id, string path) =>
        CallIndependent<JsonElement>(new { version = 1, operation = "export_optional_model_archive", id, path });
    public void InstallOptionalModelArchive(string id, string path) =>
        CallIndependent<JsonElement>(new { version = 1, operation = "install_optional_model_archive", id, path });
    public void RollbackOptionalModel(string id) =>
        Call<JsonElement>(new { version = 1, operation = "rollback_optional_model", id });
    public void VerifyOptionalModel(string id) =>
        Call<JsonElement>(new { version = 1, operation = "verify_optional_model", id });
    public bool RemoveOptionalModel(string id) =>
        Call<bool>(new { version = 1, operation = "remove_optional_model", id });

    public TranslationPackageStatus[] TranslationPackages() =>
        Call<TranslationPackageStatus[]>(new { version = 1, operation = "translation_packages" });
    public TranslationRouteStatus[] TranslationRoutes() =>
        Call<TranslationRouteStatus[]>(new { version = 1, operation = "translation_routes" });
    public TranslationReadiness GetTranslationReadiness(string source, string target) =>
        Call<TranslationReadiness>(new { version = 1, operation = "translation_readiness", source, target });
    public KnowledgePackage[] KnowledgePackages() =>
        Call<KnowledgePackage[]>(new { version = 1, operation = "knowledge_packages" });
    public KnowledgePackage InstallKnowledgePackage(string language, string path) =>
        CallIndependent<KnowledgePackage>(new { version = 1, operation = "install_knowledge_package", language, path });
    public WordLookup? LookupWord(string language, string word, WordContext? context = null) =>
        Call<WordLookup?>(new { version = 1, operation = "lookup_word", language, word, context });
    public OcrSuggestion[]? SuggestOcr(string language, string text) =>
        Call<OcrSuggestion[]?>(new { version = 1, operation = "suggest_ocr", language, text });
    public void ExportTranslationPackage(string source, string target, string path) =>
        CallIndependent<JsonElement>(new { version = 1, operation = "export_translation_package", source, target, path });
    public string InstallTranslationPackage(string path) =>
        CallIndependent<string>(new { version = 1, operation = "install_translation_package", path });
    public void RollbackTranslationPackage(string source, string target) =>
        CallIndependent<JsonElement>(new { version = 1, operation = "rollback_translation_package", source, target });
    public void VerifyTranslationPackage(string source, string target) =>
        CallIndependent<JsonElement>(new { version = 1, operation = "verify_translation_package", source, target });
    public TranslationPackageValidation ValidateTranslationPackage(string source, string target) =>
        CallIndependent<TranslationPackageValidation>(new { version = 1, operation = "validate_translation_package", source, target });
    public bool RemoveTranslationPackage(string source, string target) =>
        CallIndependent<bool>(new { version = 1, operation = "remove_translation_package", source, target });
    public void CheckTranslationDownloadSpace(string source, string target, string stagingRoot, string archiveDirectory) =>
        Call<JsonElement>(new { version = 1, operation = "check_translation_download_space", source, target, stagingRoot, archiveDirectory });
    public void CheckTranslationArchiveSpace(string directory, long bytes) =>
        Call<JsonElement>(new { version = 1, operation = "check_translation_archive_space", directory, bytes });
    public ParagraphTranslation TranslateParagraph(string id, string sentenceId, string target, bool background = false) =>
        background
            ? CallIndependent<ParagraphTranslation>(new { version = 1, operation = "translate_paragraph", id, sentenceId, target, background })
            : CallIndependent<ParagraphTranslation>(new { version = 1, operation = "translate_paragraph", id, sentenceId, target });
    public CachedTranslation ReadCachedTranslation(string id, string sentenceId, string target) =>
        CallIndependent<CachedTranslation>(new { version = 1, operation = "cached_translation", id, sentenceId, target });
    public TranslationFindPage FindTranslationPage(string id, string target, string query, int start) =>
        CallIndependent<TranslationFindPage>(new { version = 1, operation = "find_translation_page", id, target, query, start });
    public LexicalPackage[] LexicalPackages() =>
        Call<LexicalPackage[]>(new { version = 1, operation = "lexical_packages" });
    public LexicalPackage InstallLexicalPackage(string language, string path) =>
        CallIndependent<LexicalPackage>(new { version = 1, operation = "install_lexical_package", language, path });
    public DictionarySource[] DictionarySources() =>
        Call<DictionarySource[]>(new { version = 1, operation = "dictionary_sources" });
    public DictionarySourceLocation[] DictionarySourceLocations(string cardId) =>
        Call<DictionarySourceLocation[]>(new { version = 1, operation = "dictionary_source_locations", cardId });
    public Vocabulary[] DictionaryCollection(string? documentId = null) =>
        Call<Vocabulary[]>(new { version = 1, operation = "dictionary_collection", documentId });
    public DictionaryArchive ExportDictionary(string? documentId = null) =>
        Call<DictionaryArchive>(new { version = 1, operation = "export_dictionary", documentId });
    public DictionaryImport ImportDictionary(DictionaryArchive archive) =>
        Call<DictionaryImport>(new { version = 1, operation = "import_dictionary", archive });
    public string PrepareUkrainianSpeech(string text) =>
        CallIndependent<string>(new { version = 1, operation = "prepare_ukrainian_speech", text });
    public SpeechAudio SynthesizeSpeech(string id, string sentenceId, uint speaker, float rate,
        string engine = "piper-uk", int? page = null, CancellationToken cancellationToken = default) =>
        CallCancelable<SpeechAudio>(ref nextSpeechRequest, cancellationToken,
            requestId => new { version = 1, operation = "synthesize_speech", id, page, sentenceId, engine, speaker, rate, requestId },
            CancelSpeechRequest);
    public SpeechAudio SynthesizeSpeechText(string text, string language, uint speaker, float rate, string engine,
        CancellationToken cancellationToken = default) =>
        CallCancelable<SpeechAudio>(ref nextSpeechRequest, cancellationToken,
            requestId => new { version = 1, operation = "synthesize_speech_text", text, language, engine, speaker, rate, requestId },
            CancelSpeechRequest);

    public void PrefetchSpeech(string id, string sentenceId, uint speaker, float rate, string engine,
        int? page = null, CancellationToken cancellationToken = default) =>
        CallCancelable<JsonElement>(ref nextSpeechPrefetchRequest, cancellationToken,
            requestId => new { version = 1, operation = "prefetch_speech", id, page, sentenceId, engine, speaker, rate, requestId },
            requestId => new { version = 1, operation = "cancel_speech", requestId, prefetch = true });

    private static object CancelSpeechRequest(long requestId) =>
        new { version = 1, operation = "cancel_speech", requestId };

    public void ScheduleTranslationWindow(string id, string sentenceId, string revisionId, string target) =>
        Call<JsonElement>(new { version = 1, operation = "schedule_translation_window", id, sentenceId, revisionId, target });

    public ParagraphTranslation? RunTranslationWindow(string id, string sentenceId, string revisionId, string target) =>
        CallIndependent<ParagraphTranslation?>(new
        {
            version = 1, operation = "run_translation_window", id,
            expected = new { sentenceId, revisionId, target },
        });

    public MachineTranslation? TranslateSentence(string id, string sentenceId, string target) =>
        CallIndependent<MachineTranslation?>(new { version = 1, operation = "translate_sentence", id, sentenceId, target });
    public ModelPassageTranslation TranslateWithModel(string id, int? page, string sentenceId, string target,
        bool regenerate, CancellationToken cancellationToken = default) =>
        CallCancelable<ModelPassageTranslation>(ref nextModelTranslationRequest, cancellationToken,
            requestId => new { version = 1, operation = "translate_with_model", id, page, sentenceId, target, regenerate, requestId },
            requestId => new { version = 1, operation = "cancel_model_translation", requestId });

    private T CallCancelable<T>(ref long sequence, CancellationToken cancellationToken,
        Func<long?, object> request, Func<long, object> cancel)
    {
        if (!cancellationToken.CanBeCanceled) return CallIndependent<T>(request(null));
        cancellationToken.ThrowIfCancellationRequested();
        var requestId = Interlocked.Increment(ref sequence);
        using var registration = cancellationToken.Register(() =>
        {
            try { CallIndependent<JsonElement>(cancel(requestId)); }
            catch (ObjectDisposedException) { }
            catch (CoreException error) { System.Diagnostics.Debug.WriteLine(error.Message); }
        });
        try
        {
            var result = CallIndependent<T>(request(requestId));
            cancellationToken.ThrowIfCancellationRequested();
            return result;
        }
        catch (CoreException) when (cancellationToken.IsCancellationRequested)
        {
            throw new OperationCanceledException(cancellationToken);
        }
    }
    public ModelPassageTranslation? CachedModelTranslation(string id, int? page, string sentenceId, string target) =>
        CallIndependent<ModelPassageTranslation?>(new { version = 1, operation = "translate_with_model", id, page, sentenceId, target, cachedOnly = true });
    public OfflinePassageTranslation? TranslateOfflinePassage(string id, int? page, string sentenceId, string target) =>
        CallIndependent<OfflinePassageTranslation?>(new { version = 1, operation = "translate_offline_passage", id, page, sentenceId, target });
    public MachineTranslation? TranslateDictionaryGloss(string id, string source, string gloss, string target = "uk") =>
        CallIndependent<MachineTranslation?>(new { version = 1, operation = "translate_dictionary_gloss", id, source, gloss, target });
    public string?[] TranslateDictionaryGlosses(string id, string source, string target, string[] glosses) =>
        CallIndependent<string?[]>(new { version = 1, operation = "translate_dictionary_glosses", id, source, target, glosses });
    public MachineTranslation? TranslateSelection(string id, string target, string revisionId, int startUtf16, string text,
        int? page = null) => CallIndependent<MachineTranslation?>(new
        {
            version = 1, operation = "translate_selection", id, target,
            selection = new { revisionId, startUtf16, text, page },
        });
    public WordLookup? LookupDetachedPdfWord(string id, int page, int startUtf16, string text) =>
        CallIndependent<WordLookup?>(new { version = 1, operation = "lookup_detached_pdf_word", id, page, startUtf16, text });

    public Vocabulary SaveDetachedPdfWord(string id, int page, int startUtf16, string selectedText,
        string translation, string target, LemmaSnapshot? analysis, ExpressionSnapshot? expression,
        SenseSnapshot? sense = null, string? revisionId = null, string? reconstructedText = null) =>
        Call<Vocabulary>(new
        {
            version = 1, operation = "save_detached_pdf_word", id, page,
            input = new { startUtf16, selectedText, translation, target, analysis, expression, sense, revisionId, reconstructedText },
        });

    public Vocabulary SaveWord(Vocabulary entry) =>
        Call<Vocabulary>(new { version = 1, operation = "save_word", entry });
    public Vocabulary SaveSelectedWord(Vocabulary entry, string revisionId, int startUtf16, string? reconstructedText = null) =>
        Call<Vocabulary>(new { version = 1, operation = "save_word", entry, selection = new { revisionId, startUtf16, reconstructedText } });
    public Vocabulary SetVocabularyVariants(string id, string[] acceptedTranslations, string? translation = null) =>
        Call<Vocabulary>(new { version = 1, operation = "set_vocabulary_variants", id, acceptedTranslations, translation });
    public bool DeleteVocabularyCard(string id) =>
        Call<bool>(new { version = 1, operation = "delete_vocabulary_card", id });
    public ReviewEvent[] VocabularyReviewHistory(string id) =>
        Call<ReviewEvent[]>(new { version = 1, operation = "vocabulary_review_history", id });
    public void Review(string id, string direction, byte grade) =>
        Call<JsonElement>(new { version = 1, operation = "review", id, direction, grade });
    public PracticeProgress GetPractice() =>
        Call<PracticeProgress>(new { version = 1, operation = "practice" });
    public PracticeProgress SetPractice(PracticeProgress progress) =>
        Call<PracticeProgress>(new { version = 1, operation = "set_practice", progress });
    public PracticeQueue NextPracticeQuestion(ulong revision, PracticeSettings? settings) =>
        Call<PracticeQueue>(new { version = 1, operation = "next_practice_question", revision, settings });

    public PracticeAnswer AnswerPracticeQuestion(ulong revision, string? answer, uint elapsedMs) =>
        Call<PracticeAnswer>(new { version = 1, operation = "answer_practice_question", revision, answer, elapsedMs });
    public void GradePractice(ulong revision, byte grade) =>
        Call<PracticeProgress>(new { version = 1, operation = "grade_practice", revision, grade });
    public void Restore(Backup backup) => Call<JsonElement>(new { version = 1, operation = "restore", backup });

    private T Call<T>(object request)
    {
        lock (sync)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            var added = false;
            try
            {
                handle.DangerousAddRef(ref added);
                return Invoke<T>(handle.Value, request);
            }
            finally { if (added) handle.DangerousRelease(); }
        }
    }

    private T CallIndependent<T>(object request)
    {
        var added = false;
        try
        {
            lock (sync)
            {
                ObjectDisposedException.ThrowIf(disposed, this);
                handle.DangerousAddRef(ref added);
            }
            return Invoke<T>(handle.Value, request);
        }
        finally { if (added) handle.DangerousRelease(); }
    }

    private static T Invoke<T>(ulong id, object request)
    {
        var bytes = JsonSerializer.SerializeToUtf8Bytes(request, Json);
        var result = NativeMethods.reader_call(id, bytes, (ulong)bytes.Length);
        try { return Decode<T>(result); }
        finally { if (result.Allocation != 0) NativeMethods.reader_release(result.Allocation); }
    }

    private static T Decode<T>(NativeBuffer result)
    {
        if (result.Data == 0 || result.Allocation == 0 || result.Length > 256UL * 1024 * 1024)
            throw new InvalidDataException("Rust bridge returned an invalid allocation");
        var copy = new byte[checked((int)result.Length)];
        Marshal.Copy(result.Data, copy, 0, copy.Length);
        using var document = JsonDocument.Parse(copy);
        var root = document.RootElement;
        if (root.GetProperty("version").GetUInt32() != 1) throw new InvalidDataException("Unknown response version");
        if (!root.GetProperty("ok").GetBoolean())
            throw new CoreException(root.GetProperty("error").Deserialize<BridgeError>(Json)!);
        return root.GetProperty("data").Deserialize<T>(Json)!;
    }

    public void Dispose()
    {
        lock (sync)
        {
            if (disposed) return;
            disposed = true;
            handle.Dispose();
        }
    }
}
