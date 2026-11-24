using Reader.Contracts;

namespace Reader.Presentation;

public static class PdfSentenceNavigation
{
    public static int ArrivalPosition(Sentence[] sentences, OcrSpan page, int direction, string? departingSentenceId)
    {
        var step = direction > 0 ? 1 : -1;
        var fallback = -1;
        for (var index = step > 0 ? 0 : sentences.Length - 1; index >= 0 && index < sentences.Length; index += step)
        {
            var sentence = sentences[index];
            if (sentence.StartUtf16 >= page.EndUtf16 || sentence.EndUtf16 <= page.StartUtf16) continue;
            if (sentence.Id != departingSentenceId) return index;
            fallback = index;
        }
        return fallback;
    }
}
