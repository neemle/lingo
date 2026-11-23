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

    internal static async Task<string?> DetectSampleLanguageAsync(string[] samples, Func<string, Task<string?>> detect)
    {
        var votes = await Task.WhenAll(samples.Where(text => !string.IsNullOrWhiteSpace(text)).Select(detect));
        var recognized = votes.OfType<string>().ToArray();
        if (recognized.Length == 0) return await detect(string.Join("\n", samples));

        // One cover page cannot decide the language when we sampled a larger book.
        if (recognized.Length == 1 && samples.Length >= 3) return null;
        return recognized.GroupBy(language => language, StringComparer.Ordinal)
            .FirstOrDefault(group => group.Count() > recognized.Length / 2)?.Key;
    }
}
