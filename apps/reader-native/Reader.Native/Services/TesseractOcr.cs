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
        ["uk"] = "ukr", ["en"] = "eng", ["fr"] = "fra", ["de"] = "deu", ["es"] = "spa", ["it"] = "ita",
        ["pt"] = "por", ["pl"] = "pol", ["nl"] = "nld", ["cs"] = "ces", ["ro"] = "ron", ["sv"] = "swe",
    };
    private static readonly SemaphoreSlim Probe = new(1);
    private static ReadOnlySpan<byte> PngSignature => [137, 80, 78, 71, 13, 10, 26, 10];
    private static Dictionary<string, string> executableByLanguage = [];
    private static DateTimeOffset probeExpires;

    internal static async Task<string[]> InstalledLanguagesAsync() =>
        (await InstallationsAsync(default).ConfigureAwait(false)).Keys.ToArray();

    internal static async Task<OcrLine[]> RecognizeAsync(Func<Task<OcrLine[]>> native,
        Func<Task<PhotoPreview>> prepareImage, string language, CancellationToken cancellation)
    {
        cancellation.ThrowIfCancellationRequested();
        Exception? portableFailure = null;
        try
        {
            var installations = await InstallationsAsync(cancellation).ConfigureAwait(false);
            if (installations.TryGetValue(language, out var executable))
            {
                var image = await prepareImage().ConfigureAwait(false);
                if (image.Bytes.Length is < 24 or > 64 * 1024 * 1024
                    || !image.Bytes.AsSpan(0, 8).SequenceEqual(PngSignature)
                    || !image.Bytes.AsSpan(12, 4).SequenceEqual("IHDR"u8))
                    throw new InvalidDataException("Tesseract потребує коректного PNG-зображення.");
                var width = BinaryPrimitives.ReadInt32BigEndian(image.Bytes.AsSpan(16, 4));
                var height = BinaryPrimitives.ReadInt32BigEndian(image.Bytes.AsSpan(20, 4));
                if (width is < 1 or > 4096 || height is < 1 or > 4096)
                    throw new InvalidDataException("Розмір зображення для Tesseract перевищує допустимі межі.");
                var output = await ExecuteAsync(executable,
                    ["stdin", "stdout", "-l", Languages[language], "--oem", "1", "--psm", "3", "tsv"], image.Bytes,
                    16 * 1024 * 1024, TimeSpan.FromSeconds(60), cancellation).ConfigureAwait(false);
                cancellation.ThrowIfCancellationRequested();
                var lines = RustCoreClient.ParseOcrTsv(output, width, height);
                if (lines.Length > 0) return lines;
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

    private static bool CanFallback(Exception error) => error is IOException or InvalidDataException or CoreException
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
