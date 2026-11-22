using System.Net;
using System.Net.Http.Headers;
using System.Security.Cryptography;
using Reader.Contracts;

namespace Reader.Presentation;

/// <summary>Downloads pinned local model and linguistic resources.</summary>
public sealed class ModelDownload(HttpClient http)
{
    private const long AssetLimit = 400_000_000;
    private static readonly string[] AssetNames =
        ["config.json", "source.spm", "target.spm", "vocab.json", "model.safetensors"];

    public async Task FetchModelAsync(IModelTransferPort port, ModelTransferPackage package, string transportDirectory,
        string legacyDirectory,
        IProgress<long>? progress, CancellationToken cancellationToken)
    {
        Directory.CreateDirectory(transportDirectory);
        try
        {
            cancellationToken.ThrowIfCancellationRequested();
            var plan = await port.ResumeModelTransferAsync(package).ConfigureAwait(false);
            progress?.Report(plan.ReceivedBytes);
            var checkedLegacyAssets = new HashSet<string>(StringComparer.Ordinal);
            while (plan.NextRange is { } range)
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (range.From == 0 && checkedLegacyAssets.Add(range.Asset))
                {
                    var existing = Path.Combine(legacyDirectory, plan.Revision, range.Asset);
                    if (File.Exists(existing))
                    {
                        plan = await port.AdoptModelPrefixAsync(package, range.Asset, existing).ConfigureAwait(false);
                        progress?.Report(plan.ReceivedBytes);
                        continue;
                    }
                }
                var temporary = Path.Combine(transportDirectory, $"range-{Guid.NewGuid():N}.tmp");
                try
                {
                    var code = await FetchRangeAsync(plan.Revision, range, temporary, cancellationToken).ConfigureAwait(false);
                    cancellationToken.ThrowIfCancellationRequested();
                    plan = await port.AcceptModelRangeAsync(package, range, code, temporary).ConfigureAwait(false);
                    progress?.Report(plan.ReceivedBytes);
                }
                finally
                {
                    try { File.Delete(temporary); }
                    catch (IOException) { }
                    catch (UnauthorizedAccessException) { }
                }
            }
            cancellationToken.ThrowIfCancellationRequested();
            if (plan.Status != "readyToInstall")
                throw new InvalidDataException("Основні файли моделі ще не завантажені.");
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            await port.PauseModelTransferAsync(package).ConfigureAwait(false);
            throw;
        }
    }

    private async Task<int> FetchRangeAsync(string revision, ModelTransferRange range, string path,
        CancellationToken cancellationToken)
    {
        if (!Hex(revision, 40) || !Uri.TryCreate(range.Url, UriKind.Absolute, out var url)
            || url.Scheme != Uri.UriSchemeHttps || url.Host != "huggingface.co"
            || !url.AbsolutePath.Contains($"/resolve/{revision}/", StringComparison.Ordinal)
            || range.From < 0 || range.To < range.From || range.To >= range.Total
            || range.To - range.From >= 16 * 1024 * 1024)
            throw new InvalidDataException("Некоректний запит на діапазон файлу моделі.");

        var length = range.To - range.From + 1;
        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.AcceptEncoding.ParseAdd("identity");
        request.Headers.Range = new RangeHeaderValue(range.From, range.To);
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken)
            .ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        if (response.RequestMessage?.RequestUri?.Scheme != Uri.UriSchemeHttps)
            throw new InvalidDataException("Сервер спрямував завантаження на незахищену адресу.");

        if (response.StatusCode == HttpStatusCode.PartialContent)
        {
            var received = response.Content.Headers.ContentRange;
            if (received is null || received.Unit != "bytes" || received.From != range.From
                || received.To != range.To || received.Length != range.Total)
                throw new InvalidDataException("Сервер повернув некоректний діапазон файлу моделі.");
        }
        else if (response.StatusCode != HttpStatusCode.OK || range.From != 0 || length != range.Total)
        {
            throw new InvalidDataException("Сервер не повернув потрібний діапазон файлу моделі.");
        }
        if (response.Content.Headers.ContentLength is long declared && declared != length)
            throw new InvalidDataException("Розмір діапазону не відповідає запиту.");

        await using var input = await response.Content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
        await using var output = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None,
            64 * 1024, FileOptions.Asynchronous);
        var buffer = new byte[64 * 1024];
        long copied = 0;
        int count;
        while ((count = await input.ReadAsync(buffer, cancellationToken).ConfigureAwait(false)) != 0)
        {
            copied += count;
            if (copied > length) throw new InvalidDataException("Сервер надіслав зайві байти.");
            await output.WriteAsync(buffer.AsMemory(0, count), cancellationToken).ConfigureAwait(false);
        }
        if (copied != length) throw new EndOfStreamException("Діапазон завантажено не повністю.");
        return (int)response.StatusCode;
    }

    public static async Task StageOptionalModelFilesAsync(OptionalModel model, string stagingRoot,
        IReadOnlyCollection<(string Name, Func<Task<Stream>> Open)> files,
        IProgress<long>? progress, CancellationToken cancellationToken)
    {
        ValidateOptionalModel(model);
        var selected = new Dictionary<string, Func<Task<Stream>>>(StringComparer.Ordinal);
        foreach (var (name, open) in files)
        {
            var asset = model.Assets.FirstOrDefault(asset =>
                string.Equals(name, asset.Name, StringComparison.OrdinalIgnoreCase) ||
                string.Equals(name, OptionalDownloadName(asset), StringComparison.OrdinalIgnoreCase));
            if (asset is null)
                throw new InvalidDataException($"Файл «{name}» не належить до цього пакета.");
            if (!selected.TryAdd(asset.Name, open))
                throw new InvalidDataException($"Файл «{asset.Name}» вибрано двічі.");
        }
        var missing = model.Assets.Where(asset => !selected.ContainsKey(asset.Name))
            .Select(OptionalDownloadName).ToArray();
        if (missing.Length > 0)
            throw new InvalidDataException("Бракує файлів: " + string.Join(", ", missing));

        var directory = Path.Combine(stagingRoot, model.Revision);
        Directory.CreateDirectory(directory);
        var buffer = new byte[64 * 1024];
        long completed = 0;
        long reported = 0;
        foreach (var asset in model.Assets)
        {
            cancellationToken.ThrowIfCancellationRequested();
            await using var input = await selected[asset.Name]().ConfigureAwait(false);
            await using var output = new FileStream(Path.Combine(directory, asset.Name),
                FileMode.CreateNew, FileAccess.Write, FileShare.None, buffer.Length, FileOptions.Asynchronous);
            long copied = 0;
            int count;
            while ((count = await input.ReadAsync(buffer, cancellationToken).ConfigureAwait(false)) != 0)
            {
                copied += count;
                if (copied > asset.Bytes) break;
                await output.WriteAsync(buffer.AsMemory(0, count), cancellationToken).ConfigureAwait(false);
                if (completed + copied - reported < 1024 * 1024) continue;
                reported = completed + copied;
                progress?.Report(reported);
            }
            if (copied != asset.Bytes)
                throw new InvalidDataException($"Розмір файлу «{asset.Name}» не відповідає каталогу.");
            completed += copied;
            progress?.Report(completed);
            reported = completed;
        }
    }

    private static string OptionalDownloadName(OptionalModelAsset asset) =>
        Path.GetFileName(Uri.UnescapeDataString(new Uri(asset.DownloadUrl).AbsolutePath));

    public static void ClearOptionalStaging(OptionalModel model, string stagingRoot)
    {
        ValidateOptionalModel(model);
        ClearStaging(stagingRoot, model.Revision, model.Assets.Select(asset => asset.Name));
    }

    private static void ValidateOptionalModel(OptionalModel model)
    {
        if (!Hex(model.Revision, 40) || model.Assets.Length is < 1 or > 8 ||
            model.Assets.Select(asset => asset.Name).Distinct(StringComparer.Ordinal).Count() != model.Assets.Length ||
            model.Assets.Any(asset => asset.Name is not ("model.gguf" or "model.onnx" or "tokenizer.json" or "piper-uk.onnx" or "piper-uk.json"
                or "duration_predictor.onnx" or "text_encoder.onnx" or "vector_estimator.onnx"
                or "vocoder.onnx" or "tts.json" or "unicode_indexer.json" or "M1.json" or "F1.json") ||
                asset.Bytes is <= 0 or > 2_000_000_000 || !Hex(asset.Sha256, 64) ||
                !Uri.TryCreate(asset.DownloadUrl, UriKind.Absolute, out var url) ||
                url.Scheme != Uri.UriSchemeHttps || url.Host != "huggingface.co" ||
                !url.AbsolutePath.Contains($"/resolve/{model.Revision}/", StringComparison.Ordinal)))
            throw new InvalidDataException("Некоректний каталог додаткових моделей.");
    }

    public async Task<string> FetchLexicalAsync(LexicalPackage package, string stagingRoot,
        IProgress<long>? progress, CancellationToken cancellationToken)
    {
        if (!Uri.TryCreate(package.DownloadUrl, UriKind.Absolute, out var url) ||
            url.Scheme != Uri.UriSchemeHttps || url.Host != "kaikki.org" ||
            !url.AbsolutePath.StartsWith("/dictionary/", StringComparison.Ordinal) ||
            package.Bytes is <= 0 or > 600_000_000 || !Hex(package.Sha256, 64))
            throw new InvalidDataException("Некоректний каталог словникових пакетів.");

        Directory.CreateDirectory(stagingRoot);
        var path = Path.Combine(stagingRoot, $"{package.Sha256}.jsonl.gz");
        using var gate = OpenGate(stagingRoot, package.Sha256);
        await FetchAssetAsync(url, new TranslationAsset(package.Name, package.Bytes, package.Sha256),
            path, 0, progress, cancellationToken);
        return path;
    }

    public async Task<string> FetchKnowledgeAsync(KnowledgePackage package, string stagingRoot,
        CancellationToken cancellationToken, IProgress<long>? progress = null)
    {
        var url = new Uri(package.DownloadUrl);
        if (url.Scheme != Uri.UriSchemeHttps || url.Host != "raw.githubusercontent.com" ||
            !url.AbsolutePath.StartsWith("/UniversalDependencies/", StringComparison.Ordinal) ||
            package.Revision.Length != 40 || !package.Revision.All(Uri.IsHexDigit) ||
            package.Bytes <= 0 || package.Bytes > AssetLimit ||
            package.Sha256.Length != 64 || !package.Sha256.All(Uri.IsHexDigit))
            throw new InvalidDataException("Некоректний каталог граматичних пакетів.");

        var path = Path.Combine(stagingRoot, $"{package.Revision}.conllu");
        Directory.CreateDirectory(stagingRoot);
        using var gate = OpenGate(stagingRoot, package.Revision);
        await FetchAssetAsync(url, new TranslationAsset(package.Name, package.Bytes, package.Sha256),
            path, 0, progress, cancellationToken);
        progress?.Report(package.Bytes);
        return path;
    }

    public static void ClearStaging(TranslationPackageStatus package, string stagingRoot)
    {
        Validate(package);
        ClearStaging(stagingRoot, package.Revision, package.Assets.Select(asset => asset.Name));
    }

    private static void ClearStaging(string stagingRoot, string revision, IEnumerable<string> assetNames)
    {
        var directory = Path.Combine(stagingRoot, revision);
        if (!Directory.Exists(directory)) return;
        using var gate = OpenGate(stagingRoot, revision);
        foreach (var name in assetNames)
            File.Delete(Path.Combine(directory, name));
        Directory.Delete(directory);
    }

    private static FileStream OpenGate(string root, string revision) =>
        new(Path.Combine(root, $"{revision}.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);

    private async Task FetchAssetAsync(
        Uri url, TranslationAsset asset, string path,
        long completed, IProgress<long>? progress, CancellationToken cancellationToken)
    {
        var previous = File.Exists(path) ? new FileInfo(path).Length : 0;
        if (previous == asset.Bytes && await MatchesAsync(path, asset, cancellationToken)) return;
        if (previous >= asset.Bytes)
        {
            File.Delete(path);
            previous = 0;
        }

        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.AcceptEncoding.ParseAdd("identity");
        if (previous != 0) request.Headers.Range = new RangeHeaderValue(previous, null);
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
        response.EnsureSuccessStatusCode();
        if (response.StatusCode == HttpStatusCode.PartialContent)
        {
            var range = response.Content.Headers.ContentRange;
            if (range is null || range.Unit != "bytes" || range.From != previous ||
                range.To != asset.Bytes - 1 || range.Length != asset.Bytes)
            {
                File.Delete(path);
                throw new InvalidDataException("Сервер повернув некоректний діапазон. Спробуйте ще раз.");
            }
        }
        else if (response.StatusCode == HttpStatusCode.OK)
        {
            previous = 0;
        }
        else
        {
            throw new InvalidDataException("Сервер повернув непідтримувану відповідь.");
        }

        if (response.Content.Headers.ContentLength is long length && length != asset.Bytes - previous)
            throw new InvalidDataException("Розмір відповіді не збігається з каталогом.");

        await using (var input = await response.Content.ReadAsStreamAsync(cancellationToken))
        await using (var output = new FileStream(path, previous == 0 ? FileMode.Create : FileMode.Append,
            FileAccess.Write, FileShare.None, 64 * 1024, FileOptions.Asynchronous))
        {
            var buffer = new byte[64 * 1024];
            long received = previous;
            long lastReport = received;
            int count;
            while ((count = await input.ReadAsync(buffer, cancellationToken)) != 0)
            {
                if (received + count > asset.Bytes)
                {
                    await output.DisposeAsync();
                    File.Delete(path);
                    throw new InvalidDataException("Завантажений файл перевищує розмір із каталогу.");
                }
                await output.WriteAsync(buffer.AsMemory(0, count), cancellationToken);
                received += count;
                if (received - lastReport >= 1_000_000)
                {
                    progress?.Report(completed + received);
                    lastReport = received;
                }
            }
            if (received != asset.Bytes)
                throw new EndOfStreamException("Завантаження перервалося. Його можна продовжити.");
        }

        if (!await MatchesAsync(path, asset, cancellationToken))
        {
            File.Delete(path);
            throw new InvalidDataException($"Файл мовного пакета пошкоджений: {asset.Name}.");
        }
    }

    private static async Task<bool> MatchesAsync(string path, TranslationAsset asset, CancellationToken token)
    {
        await using var stream = File.OpenRead(path);
        var hash = await SHA256.HashDataAsync(stream, token);
        return Convert.ToHexString(hash).Equals(asset.Sha256, StringComparison.OrdinalIgnoreCase);
    }

    private static bool Hex(string text, int length) => text.Length == length && text.All(Uri.IsHexDigit);

    private static void Validate(TranslationPackageStatus package)
    {
        if (package.Source.Length != 2 || package.Target.Length != 2 ||
            !(package.Source + package.Target).All(char.IsAsciiLetterLower) || !Hex(package.Revision, 40) ||
            (package.Repository is { } repo && (repo.Length is < 3 or > 40 ||
                !repo.All(ch => char.IsAsciiLetterOrDigit(ch) || ch == '-'))) ||
            !package.Assets.Select(asset => asset.Name).Order(StringComparer.Ordinal).SequenceEqual(AssetNames.Order(StringComparer.Ordinal)) ||
            package.Assets.Any(asset => asset.Bytes <= 0 || asset.Bytes > AssetLimit || !Hex(asset.Sha256, 64)) ||
            package.Assets.Sum(asset => asset.Bytes) > AssetLimit)
            throw new InvalidDataException("Некоректний каталог мовних пакетів.");
    }
}
