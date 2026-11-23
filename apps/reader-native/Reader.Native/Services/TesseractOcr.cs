using System.Buffers.Binary;
using System.ComponentModel;
using System.Diagnostics;
using System.Text;
using Reader.Contracts;
using Reader.CoreClient;
using RustCoreClient = Reader.CoreClient.CoreClient;

namespace Reader.Native.Services;

internal static class TesseractOcr
{
    private static readonly IReadOnlyDictionary<string, string> Languages = new Dictionary<string, string>
    {
        ["en"] = "eng", ["fr"] = "fra", ["de"] = "deu", ["es"] = "spa", ["it"] = "ita", ["uk"] = "ukr",
        ["pt"] = "por", ["pl"] = "pol", ["nl"] = "nld", ["cs"] = "ces", ["ro"] = "ron", ["sv"] = "swe",
    };
    private static readonly SemaphoreSlim Probe = new(1);
    private static ReadOnlySpan<byte> PngSignature => [137, 80, 78, 71, 13, 10, 26, 10];
    private sealed record Installation(string Executable, string? ModelDirectory = null);
    private static Dictionary<string, Installation> executableByLanguage = [];
    private static DateTimeOffset probeExpires;

    internal static async Task<string[]> InstalledLanguagesAsync() =>
        (await InstallationsAsync(default).ConfigureAwait(false)).Keys.ToArray();

    internal static async Task<string> ReadLanguageSampleAsync(Func<Task<PhotoPreview>> prepareImage,
        CancellationToken cancellation = default)
    {
        try
        {
            var installations = await InstallationsAsync(cancellation).ConfigureAwait(false);
            var group = installations.GroupBy(item => item.Value).OrderByDescending(items => items.Count()).FirstOrDefault();
            if (group is null) return "";
            var models = string.Join("+", group.Select(item => Languages[item.Key]));
            var image = await prepareImage().ConfigureAwait(false);
            ReadPngSize(image.Bytes);
            return await ExecuteAsync(group.Key,
                ["stdin", "stdout", "-l", models, "--oem", "1", "--psm", "3"], image.Bytes,
                64 * 1024, TimeSpan.FromSeconds(12), cancellation).ConfigureAwait(false);
        }
        catch (Exception error) when (CanFallback(error))
        {
            cancellation.ThrowIfCancellationRequested();
            return "";
        }
    }

    internal static async Task<OcrLine[]> RecognizeAsync(Func<Task<OcrLine[]>> native,
        Func<Task<PhotoPreview>> prepareImage, string language, CancellationToken cancellation,
        Func<Task<OcrLine[]>>? recoverRotatedText = null)
    {
        cancellation.ThrowIfCancellationRequested();
        Exception? portableFailure = null;
        try
        {
            var installations = await InstallationsAsync(cancellation).ConfigureAwait(false);
            if (installations.TryGetValue(language, out var executable))
            {
                var image = await prepareImage().ConfigureAwait(false);
                var (width, height) = ReadPngSize(image.Bytes);
                var output = await ExecuteAsync(executable,
                    ["stdin", "stdout", "-l", Languages[language], "--oem", "1", "--psm", "3", "tsv"], image.Bytes,
                    16 * 1024 * 1024, TimeSpan.FromSeconds(60), cancellation).ConfigureAwait(false);
                string? sparse = null;
                try
                {
                    sparse = await ExecuteAsync(executable,
                        ["stdin", "stdout", "-l", Languages[language], "--oem", "1", "--psm", "11",
                         "-c", "thresholding_method=2", "tsv"], image.Bytes,
                        16 * 1024 * 1024, TimeSpan.FromSeconds(12), cancellation).ConfigureAwait(false);
                }
                catch (Exception error) when (CanFallback(error))
                {
                    Debug.WriteLine(error.Message);
                }
                cancellation.ThrowIfCancellationRequested();
                var lines = RustCoreClient.ParseOcrTsv(output, width, height, sparse);
                if (lines.Length > 0)
                {
                    if (recoverRotatedText is null) return lines;
                    try
                    {
                        var crossCheck = await recoverRotatedText().ConfigureAwait(false);
                        cancellation.ThrowIfCancellationRequested();
                        return crossCheck.Length == 0 ? lines : RustCoreClient.MergeRotatedOcr(lines, crossCheck);
                    }
                    catch (Exception error) when (CanFallback(error))
                    {
                        cancellation.ThrowIfCancellationRequested();
                        Debug.WriteLine(error.Message);
                        return lines;
                    }
                }
            }
        }
        catch (Exception error) when (CanFallback(error))
        {
            portableFailure = error;
        }

        cancellation.ThrowIfCancellationRequested();
        try
        {
            return await native().ConfigureAwait(false);
        }
        catch (Exception error) when (portableFailure is not null && CanFallback(error))
        {
            throw new InvalidOperationException($"{error.Message} Tesseract: {portableFailure.Message}", error);
        }
    }

    private static (int Width, int Height) ReadPngSize(byte[] bytes)
    {
        if (bytes.Length is < 24 or > 64 * 1024 * 1024
            || !bytes.AsSpan(0, 8).SequenceEqual(PngSignature)
            || !bytes.AsSpan(12, 4).SequenceEqual("IHDR"u8))
            throw new InvalidDataException("Tesseract потребує коректного PNG-зображення.");
        var width = BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(16, 4));
        var height = BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(20, 4));
        if (width is < 1 or > 4096 || height is < 1 or > 4096)
            throw new InvalidDataException("Розмір зображення для Tesseract перевищує допустимі межі.");
        return (width, height);
    }

    private static bool CanFallback(Exception error) => error is IOException or InvalidDataException or CoreException
        or Win32Exception or UnauthorizedAccessException or NotSupportedException or InvalidOperationException or TimeoutException
        or System.Security.SecurityException;

    private static async Task<Dictionary<string, Installation>> InstallationsAsync(CancellationToken cancellation)
    {
        await Probe.WaitAsync(cancellation).ConfigureAwait(false);
        try
        {
            if (DateTimeOffset.UtcNow < probeExpires) return executableByLanguage;
            var installed = new Dictionary<string, Installation>();
            foreach (var executable in KnownExecutables().Where(item => File.Exists(item.Executable)))
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

    private static IEnumerable<Installation> KnownExecutables()
    {
#if MACCATALYST
        var contents = Path.Combine(Foundation.NSBundle.MainBundle.BundlePath, "Contents");
        yield return new Installation(Path.Combine(contents, "Helpers", "tesseract"),
            Path.Combine(contents, "Resources", "OCR", "tessdata"));
#endif
        if (OperatingSystem.IsWindows())
        {
            foreach (var folder in new[] { Environment.SpecialFolder.ProgramFiles, Environment.SpecialFolder.ProgramFilesX86 })
            {
                var directory = Environment.GetFolderPath(folder);
                if (Path.IsPathFullyQualified(directory)) yield return new(Path.Combine(directory, "Tesseract-OCR", "tesseract.exe"));
            }
        }
        else if (OperatingSystem.IsMacCatalyst() || OperatingSystem.IsMacOS())
        {
            yield return new("/opt/homebrew/bin/tesseract");
            yield return new("/usr/local/bin/tesseract");
            yield return new("/opt/local/bin/tesseract");
        }
    }

    private static async Task<string> ExecuteAsync(Installation installation, string[] arguments, byte[]? image,
        int outputLimit, TimeSpan timeout, CancellationToken cancellation)
    {
        cancellation.ThrowIfCancellationRequested();
        var start = new ProcessStartInfo(installation.Executable)
        {
            UseShellExecute = false, CreateNoWindow = true,
            WorkingDirectory = Path.GetDirectoryName(installation.Executable)!,
            RedirectStandardInput = image is not null, RedirectStandardOutput = true, RedirectStandardError = true,
        };
        if (installation.ModelDirectory is { } modelDirectory)
        {
            start.ArgumentList.Add("--tessdata-dir");
            start.ArgumentList.Add(modelDirectory);
        }
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
            if (timedOut) throw new TimeoutException("Tesseract не відповів вчасно.", failure);
            throw;
        }

        static async Task SendAsync(Stream input, byte[] bytes, CancellationToken cancellation)
        {
            await using (input.ConfigureAwait(false))
                await input.WriteAsync(bytes, cancellation).ConfigureAwait(false);
        }
    }
}
