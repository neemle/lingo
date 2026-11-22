using System.Buffers.Binary;
using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Text;
using Reader.Contracts;

namespace Reader.Native.Services;

internal static class TesseractOcr
{
    private static readonly IReadOnlyDictionary<string, string> Languages = new Dictionary<string, string>
    {
        ["uk"] = "ukr", ["en"] = "eng", ["fr"] = "fra", ["de"] = "deu", ["es"] = "spa", ["it"] = "ita",
        ["pt"] = "por", ["pl"] = "pol", ["nl"] = "nld", ["cs"] = "ces", ["ro"] = "ron", ["sv"] = "swe",
    };
    private static readonly SemaphoreSlim Probe = new(1);
    private static ReadOnlySpan<byte> PngSignature => [137, 80, 78, 71, 13, 10, 26, 10];
    private static Dictionary<string, string> executableByLanguage = [];
    private static DateTimeOffset probeExpires;

    internal static async Task<string[]> InstalledLanguagesAsync() =>
        (await InstallationsAsync(default).ConfigureAwait(false)).Keys.ToArray();

    internal static async Task<OcrLine[]> WithFallbackAsync(Func<Task<OcrLine[]>> native,
        Func<Task<PhotoPreview>> prepareImage, string language, CancellationToken cancellation)
    {
        cancellation.ThrowIfCancellationRequested();
        Exception? nativeFailure = null;
        try
        {
            var result = await native().ConfigureAwait(false);
            if (result.Length > 0) return result;
        }
        catch (Exception error) when (error is not OperationCanceledException and not OutOfMemoryException)
        {
            nativeFailure = error;
        }

        cancellation.ThrowIfCancellationRequested();
        try
        {
            var installations = await InstallationsAsync(cancellation).ConfigureAwait(false);
            if (installations.TryGetValue(language, out var executable))
            {
                var image = await prepareImage().ConfigureAwait(false);
                if (image.Bytes.Length is < 24 or > 64 * 1024 * 1024
                    || !image.Bytes.AsSpan(0, 8).SequenceEqual(PngSignature)
                    || !image.Bytes.AsSpan(12, 4).SequenceEqual("IHDR"u8))
                    throw new InvalidDataException("Резервний OCR потребує коректного PNG-зображення.");
                var width = BinaryPrimitives.ReadInt32BigEndian(image.Bytes.AsSpan(16, 4));
                var height = BinaryPrimitives.ReadInt32BigEndian(image.Bytes.AsSpan(20, 4));
                if (width is < 1 or > 4096 || height is < 1 or > 4096)
                    throw new InvalidDataException("Розмір зображення для резервного OCR перевищує допустимі межі.");
                var output = await ExecuteAsync(executable,
                    ["stdin", "stdout", "-l", Languages[language], "--psm", "3", "tsv"], image.Bytes,
                    16 * 1024 * 1024, TimeSpan.FromSeconds(60), cancellation).ConfigureAwait(false);
                return ParseTsv(output, width, height);
            }
        }
        catch (Exception error) when (CanFallback(error))
        {
            if (nativeFailure is null) return [];
            throw new InvalidOperationException($"{nativeFailure.Message} Резервний OCR Tesseract недоступний: {error.Message}", error);
        }

        if (nativeFailure is not null)
            throw new InvalidOperationException($"{nativeFailure.Message} Для резервного OCR потрібен локальний Tesseract із даними цієї мови.", nativeFailure);
        return [];
    }

    private static bool CanFallback(Exception error) => error is IOException or InvalidDataException
        or Win32Exception or UnauthorizedAccessException or NotSupportedException or InvalidOperationException or TimeoutException
        or System.Security.SecurityException;

    private static async Task<Dictionary<string, string>> InstallationsAsync(CancellationToken cancellation)
    {
        await Probe.WaitAsync(cancellation).ConfigureAwait(false);
        try
        {
            if (DateTimeOffset.UtcNow < probeExpires) return executableByLanguage;
            var installed = new Dictionary<string, string>();
            foreach (var executable in KnownExecutables().Where(File.Exists))
            {
                try
                {
                    var output = await ExecuteAsync(executable, ["--list-langs"], null, 64 * 1024,
                        TimeSpan.FromSeconds(3), cancellation).ConfigureAwait(false);
                    var tags = output.Split('\n').Select(line => line.Trim()).ToHashSet(StringComparer.Ordinal);
                    foreach (var (language, tag) in Languages)
                        if (tags.Contains(tag)) installed.TryAdd(language, executable);
                    if (installed.Count == Languages.Count) break;
                }
                catch (Exception error) when (CanFallback(error)) { }
            }
            executableByLanguage = installed;
            probeExpires = DateTimeOffset.UtcNow.AddSeconds(30);
            return installed;
        }
        finally { Probe.Release(); }
    }

    private static IEnumerable<string> KnownExecutables()
    {
        if (OperatingSystem.IsWindows())
        {
            foreach (var folder in new[] { Environment.SpecialFolder.ProgramFiles, Environment.SpecialFolder.ProgramFilesX86 })
            {
                var directory = Environment.GetFolderPath(folder);
                if (Path.IsPathFullyQualified(directory)) yield return Path.Combine(directory, "Tesseract-OCR", "tesseract.exe");
            }
        }
        else if (OperatingSystem.IsMacCatalyst() || OperatingSystem.IsMacOS())
        {
            yield return "/opt/homebrew/bin/tesseract";
            yield return "/usr/local/bin/tesseract";
            yield return "/opt/local/bin/tesseract";
        }
    }

    private static async Task<string> ExecuteAsync(string executable, string[] arguments, byte[]? image,
        int outputLimit, TimeSpan timeout, CancellationToken cancellation)
    {
        cancellation.ThrowIfCancellationRequested();
        var start = new ProcessStartInfo(executable)
        {
            UseShellExecute = false, CreateNoWindow = true,
            WorkingDirectory = Path.GetDirectoryName(executable)!,
            RedirectStandardInput = image is not null, RedirectStandardOutput = true, RedirectStandardError = true,
        };
        foreach (var argument in arguments) start.ArgumentList.Add(argument);
        start.Environment["OMP_THREAD_LIMIT"] = "2";
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Не вдалося запустити Tesseract.");
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        deadline.CancelAfter(timeout);
        var readOutput = NativeFiles.ReadBoundedAsync(process.StandardOutput.BaseStream, outputLimit, deadline.Token);
        var readErrors = NativeFiles.ReadBoundedAsync(process.StandardError.BaseStream, 64 * 1024, deadline.Token);
        var send = image is null ? Task.CompletedTask : SendAsync(process.StandardInput.BaseStream, image, deadline.Token);
        var exit = process.WaitForExitAsync(deadline.Token);
        var tasks = new List<Task> { readOutput, readErrors, send, exit };
        try
        {
            while (tasks.Count > 0)
            {
                var done = await Task.WhenAny(tasks).ConfigureAwait(false);
                await done.ConfigureAwait(false);
                tasks.Remove(done);
            }
            if (process.ExitCode != 0) throw new InvalidOperationException($"Tesseract завершився з кодом {process.ExitCode}.");
            return Encoding.UTF8.GetString(await readOutput.ConfigureAwait(false));
        }
        catch (Exception failure)
        {
            var timedOut = failure is OperationCanceledException && deadline.IsCancellationRequested;
            deadline.Cancel();
            try { if (!process.HasExited) process.Kill(); }
            catch (Exception error) when (error is InvalidOperationException or Win32Exception) { }
            try { await Task.WhenAll(tasks).ConfigureAwait(false); }
            catch (Exception error) when (error is OperationCanceledException || CanFallback(error)) { }
            if (cancellation.IsCancellationRequested) cancellation.ThrowIfCancellationRequested();
            if (timedOut) throw new TimeoutException("Резервний OCR не відповів вчасно.", failure);
            throw;
        }

        static async Task SendAsync(Stream input, byte[] bytes, CancellationToken cancellation)
        {
            await using (input.ConfigureAwait(false))
                await input.WriteAsync(bytes, cancellation).ConfigureAwait(false);
        }
    }

    private static OcrLine[] ParseTsv(string tsv, int imageWidth, int imageHeight)
    {
        using var rows = new StringReader(tsv);
        if (rows.ReadLine()?.StartsWith("level\tpage_num\tblock_num\tpar_num\tline_num\t", StringComparison.Ordinal) != true)
            throw new InvalidDataException("Tesseract повернув некоректний формат TSV.");

        var groups = new Dictionary<(int Block, int Paragraph, int Line), List<(OcrWord Word, double? Confidence)>>();
        var wordCount = 0;
        while (rows.ReadLine() is { } row)
        {
            if (!row.StartsWith("5\t", StringComparison.Ordinal)) continue;
            var fields = row.Split('\t', 12);
            if (fields.Length != 12 || fields[1] != "1" || !Integer(2, out var block) || !Integer(3, out var paragraph)
                || !Integer(4, out var line) || !Integer(6, out var x) || !Integer(7, out var y)
                || !Integer(8, out var width) || !Integer(9, out var height)) continue;
            var text = string.Join(' ', fields[11].Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
            if (text.Length == 0 || text.Contains('\0') || Encoding.UTF8.GetByteCount(text) > 8192) continue;
            if (PhotoGeometry.Word(text, (double)x / imageWidth, (double)y / imageHeight,
                (x + (double)width) / imageWidth, (y + (double)height) / imageHeight, null) is not { } word) continue;
            double? confidence = double.TryParse(fields[10], NumberStyles.Float, CultureInfo.InvariantCulture, out var score)
                                 && score is >= 0 and <= 100 ? score / 100 : null;
            var key = (block, paragraph, line);
            if (!groups.TryGetValue(key, out var words)) groups.Add(key, words = []);
            words.Add((word, confidence));
            if (++wordCount > 65_536 || groups.Count > 8192)
                throw new InvalidDataException("Результат резервного OCR перевищує допустимий розмір.");

            bool Integer(int index, out int value) => int.TryParse(fields[index], NumberStyles.Integer,
                CultureInfo.InvariantCulture, out value);
        }

        var lines = new List<OcrLine>(groups.Count);
        foreach (var (_, words) in groups.OrderBy(group => group.Key))
        {
            var text = string.Join(' ', words.Select(item => item.Word.Text));
            if (Encoding.UTF8.GetByteCount(text) > 8192)
                throw new InvalidDataException("Рядок резервного OCR завеликий.");
            if (PhotoGeometry.Line(text, words.Min(item => item.Word.X), words.Min(item => item.Word.Y),
                words.Max(item => item.Word.X + item.Word.Width), words.Max(item => item.Word.Y + item.Word.Height),
                words.Average(item => item.Confidence)) is not { } line) continue;
            var offset = 0;
            var tokens = new OcrWord[words.Count];
            for (var index = 0; index < words.Count; index++)
            {
                var word = words[index].Word;
                tokens[index] = word with { RawStartUtf16 = offset };
                offset += word.Text.Length + 1;
            }
            lines.Add(line with { Words = tokens });
        }
        return lines.ToArray();
    }
}
