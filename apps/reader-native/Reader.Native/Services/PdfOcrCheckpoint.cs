using System.Security.Cryptography;
using System.Text.Json;
using Reader.Contracts;
using Reader.Presentation;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Native.Services;

internal sealed record PdfOcrCheckpoint(string Id, string Hash, string Name, string Language, int Pages,
    int[] MissingPages, int SavedPages, string State)
{
    private const int MetadataLimit = 16 * 1024;
    private const int PageResultLimit = 32 * 1024 * 1024;
    private const string EngineVersion = "pdf-native-with-tesseract/1";
    private static readonly object Staging = new();
    private static string Root => Path.Combine(FileSystem.AppDataDirectory, "pdf-ocr");
    private string Folder => Path.Combine(Root, $"{Hash}-{Language}");

    internal static string SourceHash(byte[] bytes) => Convert.ToHexStringLower(SHA256.HashData(bytes));

    internal static PdfOcrCheckpoint Create(string name, byte[] bytes, string language, int pages, int[] missingPages)
    {
        if (bytes.Length is 0 or > Client.MaxImportBytes)
            throw new InvalidDataException("Параметри розпізнавання PDF некоректні.");
        var hash = SourceHash(bytes);
        var input = new LegacyJob(hash, Path.GetFileName(name), language, pages, missingPages.Distinct().Order().ToArray());
        if (!input.IsValid()) throw new InvalidDataException("Параметри розпізнавання PDF некоректні.");
        lock (Staging)
        {
            AdoptLegacy();
            Stage(input.Folder, bytes, hash);
            return CreateJob(input, EngineVersion, revive: true);
        }
    }

#if MACCATALYST
    internal static PdfOcrCheckpoint Create(string name, string source, string language, int pages, int[] missingPages)
    {
        var hash = HashFile(source);
        var input = new LegacyJob(hash, Path.GetFileName(name), language, pages, missingPages.Distinct().Order().ToArray());
        if (!input.IsValid()) throw new InvalidDataException("Параметри розпізнавання PDF некоректні.");
        lock (Staging)
        {
            AdoptLegacy();
            Directory.CreateDirectory(input.Folder);
            var original = Path.Combine(input.Folder, "original.pdf");
            if (!File.Exists(original) || HashFile(original) != hash)
            {
                var temporary = original + ".tmp";
                try
                {
                    File.Copy(source, temporary, overwrite: true);
                    using (var copied = File.Open(temporary, FileMode.Open, FileAccess.ReadWrite, FileShare.None))
                        copied.Flush(flushToDisk: true);
                    if (HashFile(temporary) != hash)
                        throw new InvalidDataException("Локальна копія PDF змінилася під час збереження.");
                    File.Move(temporary, original, overwrite: true);
                }
                finally { File.Delete(temporary); }
            }
            return CreateJob(input, EngineVersion, revive: true);
        }
    }

    internal bool UsesFile => new FileInfo(Path.Combine(Folder, "original.pdf")).Length > Client.MaxImportBytes;

    internal string ReadOriginalPath()
    {
        var original = Path.Combine(Folder, "original.pdf");
        if (HashFile(original) != Hash) throw new InvalidDataException("Збережена копія PDF пошкоджена.");
        return original;
    }

    private static string HashFile(string path)
    {
        using var source = File.OpenRead(path);
        if (source.Length is <= 0 or > Client.MaxPdfBytes)
            throw new InvalidDataException("Розмір PDF виходить за межі дозволеного.");
        return Convert.ToHexStringLower(SHA256.HashData(source));
    }
#endif

    internal static PdfOcrCheckpoint[] List()
    {
        lock (Staging)
        {
            AdoptLegacy();
            var jobs = Job<PdfOcrCheckpoint[]>(new { action = "list", includeTerminal = true });
            foreach (var job in jobs.Where(job => job.State is "completed" or "canceled")) Retire(job.Folder);
            return jobs.Where(job => job.State is not ("completed" or "canceled"))
                .OrderBy(job => job.Name, StringComparer.CurrentCultureIgnoreCase).ToArray();
        }
    }

    internal byte[] ReadOriginal()
    {
        var bytes = ReadBounded(Path.Combine(Folder, "original.pdf"), Client.MaxImportBytes);
        if (SourceHash(bytes) != Hash) throw new InvalidDataException("Збережена копія PDF пошкоджена.");
        return bytes;
    }

    internal OcrLine[]? ReadPage(int page) => Job<OcrLine[]?>(new { action = "read_page", id = Id, page });

    internal void SavePage(int page, OcrLine[] lines, bool adopt = false) =>
        Job<JsonElement>(new { action = "save_page", id = Id, page, lines, adopt });

    internal void Start() => SetState("running");
    internal void Pause() => SetState("pending");
    internal void Fail() => SetState("failed");

    internal void Cancel()
    {
        lock (Staging)
        {
            SetState("canceled");
            Retire(Folder);
        }
    }

    internal void Complete(string documentId)
    {
        lock (Staging)
        {
            SetState("completed", documentId);
            Retire(Folder);
        }
    }

    private void SetState(string state, string? documentId = null) =>
        Job<JsonElement>(new { action = "set_state", id = Id, state, documentId });

    private static T Job<T>(object action) => AppState.Session.PdfOcrJob<T>(action);

    private static PdfOcrCheckpoint CreateJob(LegacyJob input, string engineVersion, bool revive) =>
        Job<PdfOcrCheckpoint>(new
        {
            action = "create", input.Hash, input.Name, input.Language, engineVersion,
            input.Pages, input.MissingPages, revive,
        });

    private static void AdoptLegacy()
    {
        if (!Directory.Exists(Root)) return;
        foreach (var retired in Directory.GetDirectories(Root, ".completed-*")) DeleteRetired(retired);
        foreach (var folder in Directory.GetDirectories(Root))
        {
            var metadata = Path.Combine(folder, "job.json");
            if (!File.Exists(metadata)) continue;
            var input = ReadLegacy<LegacyJob>(metadata, MetadataLimit);
            var key = Path.GetFileName(folder);
            if (input is null || !input.IsValid() || (key != input.Hash && key != $"{input.Hash}-{input.Language}")) continue;
            var original = Path.Combine(folder, "original.pdf");
            var bytes = ReadLegacyBytes(original, Client.MaxImportBytes);
            if (bytes is null || SourceHash(bytes) != input.Hash) continue;
            var job = CreateJob(input, "legacy-native/1", revive: false);
            if (job.State is "completed" or "canceled")
            {
                Retire(folder);
                continue;
            }
            if (folder != job.Folder) Stage(job.Folder, bytes, input.Hash);
            foreach (var page in input.MissingPages.Intersect(job.MissingPages))
            {
                var path = Path.Combine(folder, $"page-{page:D2}.json");
                var lines = ReadLegacy<OcrLine[]>(path, PageResultLimit);
                if (lines is not null && lines.All(line => line is not null && line.Page == page) && job.ReadPage(page) is null)
                    job.SavePage(page, lines, adopt: true);
            }
            File.Delete(metadata);
            if (folder != job.Folder) Retire(folder);
            else
                foreach (var page in input.MissingPages) File.Delete(Path.Combine(folder, $"page-{page:D2}.json"));
        }
    }

    private static void Stage(string folder, byte[] bytes, string hash)
    {
        Directory.CreateDirectory(folder);
        var original = Path.Combine(folder, "original.pdf");
        if (ReadLegacyBytes(original, Client.MaxImportBytes) is { } staged && SourceHash(staged) == hash) return;
        var temporary = original + ".tmp";
        using (var stream = File.Create(temporary))
        {
            stream.Write(bytes);
            stream.Flush(flushToDisk: true);
        }
        File.Move(temporary, original, overwrite: true);
    }

    private static T? ReadLegacy<T>(string path, int maximum) where T : class
    {
        try { return NativeFiles.Decode<T>(ReadBounded(path, maximum)); }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or JsonException)
        { return null; }
    }

    private static byte[]? ReadLegacyBytes(string path, int maximum)
    {
        try { return ReadBounded(path, maximum); }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException) { return null; }
    }

    private static byte[] ReadBounded(string path, int maximum)
    {
        using var stream = File.OpenRead(path);
        if (stream.Length is <= 0 || stream.Length > maximum)
            throw new InvalidDataException("Розмір файлу OCR виходить за межі дозволеного.");
        var bytes = new byte[checked((int)stream.Length)];
        stream.ReadExactly(bytes);
        return bytes;
    }

    private static void Retire(string folder)
    {
        if (!Directory.Exists(folder)) return;
        var retired = Path.Combine(Root, $".completed-{Guid.NewGuid():N}");
        try { Directory.Move(folder, retired); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { return; }
        DeleteRetired(retired);
    }

    private static void DeleteRetired(string folder)
    {
        try { Directory.Delete(folder, recursive: true); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
    }

    private sealed record LegacyJob(string Hash, string Name, string Language, int Pages, int[] MissingPages)
    {
        internal string Folder => Path.Combine(Root, $"{Hash}-{Language}");

        internal bool IsValid() => Hash is { Length: 64 } && Hash.All(Uri.IsHexDigit)
            && !string.IsNullOrEmpty(Name) && Name.Length <= 4096 && Path.GetFileName(Name) == Name
            && Path.GetExtension(Name).Equals(".pdf", StringComparison.OrdinalIgnoreCase)
            && DocumentLanguages.All.Any(language => language.Code == Language)
            && Pages is >= 1 and <= PdfRecognition.MaxPages && MissingPages is { Length: > 0 }
            && MissingPages.All(page => page >= 0 && page < Pages)
            && MissingPages.SequenceEqual(MissingPages.Distinct().Order());
    }
}
