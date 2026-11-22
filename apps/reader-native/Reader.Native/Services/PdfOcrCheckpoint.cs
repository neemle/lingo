using System.Security.Cryptography;
using System.Text.Json;
using Reader.Contracts;
using Reader.Presentation;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Native.Services;

internal sealed record PdfOcrCheckpoint(string Hash, string Name, string Language, int Pages, int[] MissingPages)
{
    private const int MetadataLimit = 16 * 1024;
    private const int PageResultLimit = 32 * 1024 * 1024;
    private static string Root => Path.Combine(FileSystem.AppDataDirectory, "pdf-ocr");
    private string Folder => Path.Combine(Root, $"{Hash}-{Language}");
    private string PagePath(int page) => Path.Combine(Folder, $"page-{page:D2}.json");

    internal static string SourceHash(byte[] bytes) => Convert.ToHexStringLower(SHA256.HashData(bytes));

    internal static PdfOcrCheckpoint Create(string name, byte[] bytes, string language, int pages, int[] missingPages)
    {
        if (bytes.Length is 0 or > Client.MaxImportBytes)
            throw new InvalidDataException("Параметри розпізнавання PDF некоректні.");
        var hash = SourceHash(bytes);
        var folder = Path.Combine(Root, $"{hash}-{language}");
        var existing = Read(folder);
        if (existing is not null)
        {
            // Reselecting the same source can repair an unreadable saved original.
            try { _ = existing.ReadOriginal(); }
            catch (Exception error) when (error is IOException or InvalidDataException)
            {
                WriteAtomic(Path.Combine(folder, "original.pdf"), bytes);
            }
            return existing;
        }

        var job = new PdfOcrCheckpoint(hash, Path.GetFileName(name), language, pages,
            missingPages.Distinct().Order().ToArray());
        if (!job.IsValid(hash))
            throw new InvalidDataException("Параметри розпізнавання PDF некоректні.");
        Directory.CreateDirectory(folder);
        WriteAtomic(Path.Combine(folder, "original.pdf"), bytes);
        WriteAtomic(Path.Combine(folder, "job.json"), NativeFiles.Encode(job));
        return job;
    }

    internal static PdfOcrCheckpoint[] List()
    {
        if (!Directory.Exists(Root)) return [];
        foreach (var retired in Directory.EnumerateDirectories(Root, ".completed-*")) DeleteRetired(retired);
        return Directory.EnumerateDirectories(Root).Select(Read).OfType<PdfOcrCheckpoint>()
            .OrderBy(job => job.Name, StringComparer.CurrentCultureIgnoreCase).ToArray();
    }

    internal int SavedPages => MissingPages.Count(page => File.Exists(PagePath(page)));

    internal byte[] ReadOriginal()
    {
        var bytes = ReadBounded(Path.Combine(Folder, "original.pdf"), Client.MaxImportBytes);
        if (SourceHash(bytes) != Hash) throw new InvalidDataException("Збережена копія PDF пошкоджена.");
        return bytes;
    }

    internal OcrLine[]? ReadPage(int page)
    {
        try
        {
            var lines = NativeFiles.Decode<OcrLine[]>(ReadBounded(PagePath(page), PageResultLimit));
            return lines.All(line => line is not null && line.Page == page) ? lines : null;
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or JsonException)
        {
            return null;
        }
    }

    internal void SavePage(int page, OcrLine[] lines)
    {
        var bytes = NativeFiles.Encode(lines);
        if (bytes.Length > PageResultLimit) throw new InvalidDataException("Результат OCR сторінки завеликий.");
        WriteAtomic(PagePath(page), bytes);
    }

    internal void Complete()
    {
        if (!Directory.Exists(Folder)) return;
        var retired = Path.Combine(Root, $".completed-{Guid.NewGuid():N}");
        try { Directory.Move(Folder, retired); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { return; }
        DeleteRetired(retired);
    }

    private static PdfOcrCheckpoint? Read(string folder)
    {
        var key = Path.GetFileName(folder);
        if (key.Length != 67 || key[64] != '-' || !key[..64].All(Uri.IsHexDigit)) return null;
        try
        {
            var job = NativeFiles.Decode<PdfOcrCheckpoint>(ReadBounded(Path.Combine(folder, "job.json"), MetadataLimit));
            return job.IsValid(key[..64]) && job.Language == key[65..] &&
                File.Exists(Path.Combine(folder, "original.pdf")) ? job : null;
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or JsonException)
        {
            return null;
        }
    }

    private bool IsValid(string hash) => Hash == hash && !string.IsNullOrEmpty(Name) && Name.Length <= 4096
        && Path.GetFileName(Name) == Name && Path.GetExtension(Name).Equals(".pdf", StringComparison.OrdinalIgnoreCase)
        && DocumentLanguages.All.Any(language => language.Code == Language)
        && Pages is >= 1 and <= PdfRecognition.MaxPages && MissingPages is { Length: > 0 }
        && MissingPages.All(page => page >= 0 && page < Pages)
        && MissingPages.SequenceEqual(MissingPages.Distinct().Order());

    private static byte[] ReadBounded(string path, int maximum)
    {
        using var stream = File.OpenRead(path);
        if (stream.Length is <= 0 || stream.Length > maximum)
            throw new InvalidDataException("Розмір файлу OCR виходить за межі дозволеного.");
        var bytes = new byte[checked((int)stream.Length)];
        stream.ReadExactly(bytes);
        return bytes;
    }

    private static void WriteAtomic(string path, byte[] bytes)
    {
        var temporary = path + ".tmp";
        using (var stream = File.Create(temporary))
        {
            stream.Write(bytes);
            stream.Flush(flushToDisk: true);
        }
        File.Move(temporary, path, overwrite: true);
    }

    private static void DeleteRetired(string folder)
    {
        try { Directory.Delete(folder, recursive: true); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
    }
}
