namespace Reader.Native.Services;

internal static class PdfRecognition
{
    // The Rust bridge represents PDF page counts and indices as unsigned 16-bit integers.
    internal const int MaxPages = ushort.MaxValue;

    internal static void CheckPageCount(int count)
    {
        if (count is < 1 or > MaxPages)
            throw new InvalidDataException($"Для розпізнавання PDF має містити від 1 до {MaxPages} сторінок.");
    }

    internal static async Task<string?> DetectSampleLanguageAsync(string[] textLayers, Func<string, Task<string?>> detect,
        CancellationToken cancellation, params Func<int, int, Task<string[]>>[] readOcr)
    {
        var votes = new string?[textLayers.Length];
        var bestSamples = (string[])textLayers.Clone();
        var bodyCount = Math.Min(3, textLayers.Length);

        async Task<string?> ProbeAsync(int start, int count)
        {
            var end = start + count;
            var window = votes.AsMemory(start, count);
            await AddLanguageVotesAsync(textLayers[start..end], window, detect);
            cancellation.ThrowIfCancellationRequested();
            var language = SampleLanguage(votes.AsSpan(0, end));
            if (language is not null) return language;

            foreach (var read in readOcr)
            {
                if (Array.IndexOf(votes, null, start, count) < 0) break;
                var samples = await read(start, count);
                await AddLanguageVotesAsync(samples, window, detect);
                cancellation.ThrowIfCancellationRequested();
                language = SampleLanguage(votes.AsSpan(0, end));
                if (language is not null) return language;

                for (var i = 0; i < samples.Length; i++)
                    if (samples[i].Length > bestSamples[start + i].Length)
                        bestSamples[start + i] = samples[i];
            }

            // Detect short snippets jointly when no individual page supplied a confident vote.
            if (votes.Take(end).Any(vote => vote is not null)) return null;
            var snippets = bestSamples.Take(end).Where(sample => !string.IsNullOrWhiteSpace(sample)).ToArray();
            return snippets.Length >= (end == 1 ? 1 : 2) ? await detect(string.Join("\n", snippets)) : null;
        }

        if (bodyCount == 0) return null;
        var language = await ProbeAsync(0, bodyCount);
        if (language is not null || bodyCount == textLayers.Length) return language;
        return await ProbeAsync(bodyCount, textLayers.Length - bodyCount);
    }

    private static async Task AddLanguageVotesAsync(string[] samples, Memory<string?> votes,
        Func<string, Task<string?>> detect)
    {
        await Task.WhenAll(samples.Select(async (sample, index) =>
        {
            if (votes.Span[index] is null && !string.IsNullOrWhiteSpace(sample))
            {
                var language = await detect(sample).ConfigureAwait(false);
                votes.Span[index] = language;
            }
        })).ConfigureAwait(false);
    }

    private static string? SampleLanguage(ReadOnlySpan<string?> votes)
    {
        // One cover page cannot decide the language when we sampled a larger book.
        var recognized = votes.ToArray().OfType<string>().ToArray();
        if (recognized.Length == 1 && votes.Length >= 3) return null;
        return recognized.GroupBy(language => language, StringComparer.Ordinal)
            .FirstOrDefault(group => group.Count() > recognized.Length / 2)?.Key;
    }
}
