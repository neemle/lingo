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
    public const int MaxImportBytes = 20 * 1024 * 1024;

    public CoreClient(string databasePath)
    {
        if (NativeMethods.reader_abi_version() != 1) throw new NotSupportedException("Rust ABI v1 required");
        var opened = Invoke<OpenResult>(0, new { version = 1, operation = "open", path = databasePath });
        handle = new StoreHandle(opened.Handle);
    }

    public Snapshot Snapshot() => Call<Snapshot>(new { version = 1, operation = "snapshot" });
    public Backup Export() => Call<Backup>(new { version = 1, operation = "export" });
    public Original Original(string id) => Call<Original>(new { version = 1, operation = "original", id });
    public Document Import(string name, ReadOnlySpan<byte> bytes, string language)
    {
        if (bytes.Length is 0 or > MaxImportBytes)
            throw new ArgumentOutOfRangeException(nameof(bytes), "Import requires 1..20MiB");
        return Call<Document>(new { version = 1, operation = "import", name,
            bytes = bytes.ToArray().Select(value => (int)value).ToArray(), language });
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
    public void SetDocumentLanguage(string id, string language) =>
        Call<JsonElement>(new { version = 1, operation = "set_document_language", id, language });
    public SpeechResume? SpeechResume(string id) =>
        Call<SpeechResume?>(new { version = 1, operation = "speech_resume", id });
    public void SetSpeechResume(string id, string sentenceId, string textHash, int? offsetUtf16) =>
        Call<JsonElement>(new { version = 1, operation = "set_speech_resume", id, sentenceId, textHash, offsetUtf16 });
    public void SetBookmark(string id, string sentenceId, bool saved) =>
        Call<JsonElement>(new { version = 1, operation = "set_bookmark", id, sentenceId, saved });
    public void SetReviewedTranslation(string id, string sentenceId, string target, string text) =>
        Call<JsonElement>(new { version = 1, operation = "set_reviewed_translation", id, sentenceId, target, text });
    public TranslationPackageStatus[] TranslationPackages() =>
        Call<TranslationPackageStatus[]>(new { version = 1, operation = "translation_packages" });
    public TranslationReadiness GetTranslationReadiness(string source, string target) =>
        Call<TranslationReadiness>(new { version = 1, operation = "translation_readiness", source, target });
    public KnowledgePackage[] KnowledgePackages() =>
        Call<KnowledgePackage[]>(new { version = 1, operation = "knowledge_packages" });
    public KnowledgePackage InstallKnowledgePackage(string language, string path) =>
        Call<KnowledgePackage>(new { version = 1, operation = "install_knowledge_package", language, path });
    public WordLookup? LookupWord(string language, string word, WordContext? context = null) =>
        Call<WordLookup?>(new { version = 1, operation = "lookup_word", language, word, context });
    public OcrSuggestion[]? SuggestOcr(string language, string text) =>
        Call<OcrSuggestion[]?>(new { version = 1, operation = "suggest_ocr", language, text });
    public string InstallTranslationPackage(string path) =>
        Call<string>(new { version = 1, operation = "install_translation_package", path });
    public MachineTranslation? TranslateSentence(string id, string sentenceId, string target) =>
        Call<MachineTranslation?>(new { version = 1, operation = "translate_sentence", id, sentenceId, target });
    public Vocabulary SaveWord(Vocabulary entry) =>
        Call<Vocabulary>(new { version = 1, operation = "save_word", entry });
    public void Review(string id, string direction, byte grade) =>
        Call<JsonElement>(new { version = 1, operation = "review", id, direction, grade });
    public PracticeProgress GetPractice() =>
        Call<PracticeProgress>(new { version = 1, operation = "practice" });
    public PracticeProgress SetPractice(PracticeProgress progress) =>
        Call<PracticeProgress>(new { version = 1, operation = "set_practice", progress });
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
