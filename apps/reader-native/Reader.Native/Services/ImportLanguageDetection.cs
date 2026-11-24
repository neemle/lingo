using Reader.Contracts;
using Reader.Presentation;
#if MACCATALYST || IOS
using Foundation;
using NaturalLanguage;
#endif

namespace Reader.Native.Services;

internal static class ImportLanguageDetection
{
    internal static async Task<string?> DetectAsync(ReaderSession session, string text) =>
        Apply(await session.DetectImportLanguageAsync(text).ConfigureAwait(false));

    internal static async Task<string?> DetectDocumentAsync(ReaderSession session, string name, byte[] bytes) =>
        Apply(await session.DetectDocumentImportLanguageAsync(name, bytes).ConfigureAwait(false));

    private static string? Apply(ImportLanguageSample result)
    {
        if (result.Language is { } language) return language;
#if MACCATALYST || IOS
        var sample = result.Sample;
        if (sample.Where(char.IsLetter).Take(14).Count() < 14 ||
            sample.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
                .Count(word => word.Any(char.IsLetter)) < 4)
            return null;

        using var pool = new NSAutoreleasePool();
        using var recognizer = new NLLanguageRecognizer();
        recognizer.Process(sample);
        using var hypotheses = recognizer.GetNativeLanguageHypotheses(1);
        foreach (var candidate in hypotheses)
        {
            if (candidate.Key is not NSString tag || candidate.Value is not NSNumber confidence) continue;
            var code = tag.ToString();
            if (confidence.DoubleValue >= 0.97 && DocumentLanguages.All.Any(item => item.Code == code))
                return code;
        }
#endif
        return null;
    }
}
