using Reader.Contracts;

namespace Reader.Presentation;

public static class DocumentFind
{
    public const int PageSize = 30;
    public sealed record PdfFindHit(int Page, TextSelection Selection, string Text);
    public sealed record PdfFindCursor(int Page, int Offset);

    public static bool Contains(string text, string query)
    {
        var needle = Normalize(query.Trim());
        return needle.Length > 0 && Normalize(text).Contains(needle, StringComparison.OrdinalIgnoreCase);
    }

    public static (int[] Positions, int? NextStart) FindPage(Sentence[] sentences, string query,
        int start, CancellationToken cancellationToken, string? pdfLanguage = null)
    {
        var needle = Normalize(query.Trim());
        if (needle.Length == 0) return ([], null);

        var positions = new List<int>(PageSize);
        for (var position = Math.Max(0, start); position < sentences.Length; position++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var text = Normalize(sentences[position].Text);
            if (!text.Contains(needle, StringComparison.OrdinalIgnoreCase) &&
                (pdfLanguage is null || !sentences[position].Text.Contains('\n') ||
                    !PdfSourceMatches(sentences[position].Text, query, pdfLanguage).Any())) continue;
            if (positions.Count == PageSize) return (positions.ToArray(), position);
            positions.Add(position);
        }
        return (positions.ToArray(), null);
    }

    public static (PdfFindHit[] Matches, PdfFindCursor? Next) FindPdfPage(Document document, string query,
        int startPage, int startUtf16, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (document.PdfPageStarts is not { } starts || string.IsNullOrWhiteSpace(query)) return ([], null);

        var hits = new List<PdfFindHit>(PageSize);
        for (var page = Math.Max(0, startPage); page < starts.Length; page++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (starts[page] is not int pageStart) continue;
            var end = document.Text.Length;
            for (var next = page + 1; next < starts.Length; next++)
            {
                if (starts[next] is not int nextStart) continue;
                end = nextStart;
                break;
            }
            if (pageStart < 0 || end <= pageStart || end > document.Text.Length) continue;

            var source = document.Text[pageStart..end];
            var minimum = page == startPage ? startUtf16 : pageStart;
            foreach (var hit in PdfPageHits(page, source, query, document.Language, pageStart, minimum, cancellationToken))
            {
                if (hits.Count == PageSize)
                    return (hits.ToArray(), new PdfFindCursor(page, hit.Selection.StartUtf16));
                hits.Add(hit);
            }
        }
        return (hits.ToArray(), null);
    }

    public static IEnumerable<PdfFindHit> PdfPageHits(int page, string source, string query, string language,
        int pageStart, int minimum, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var lastStart = -1;
        int[]? excerptStarts = null;
        foreach (var selection in PdfSourceMatches(source, query, language, pageStart))
        {
            cancellationToken.ThrowIfCancellationRequested();
            // Raw text and a rejoined word may identify the same physical occurrence.
            if (selection.StartUtf16 < minimum || selection.StartUtf16 == lastStart) continue;
            lastStart = selection.StartUtf16;
            excerptStarts ??= System.Globalization.StringInfo.ParseCombiningCharacters(source);
            yield return new(page, selection, PdfExcerpt(source, excerptStarts, selection.StartUtf16 - pageStart));
        }
    }

    private static string PdfExcerpt(string source, int[] starts, int offset)
    {
        var first = Math.Max(0, Array.BinarySearch(starts, offset) - 8);
        var end = first + 240;
        var tail = end < starts.Length;
        return (first > 0 ? "…" : "") + source[starts[first]..(tail ? starts[end] : source.Length)] + (tail ? "…" : "");
    }

    public static string MatchingExcerpt(string text, string query)
    {
        return SourceMatch(text, query) is { } match ? MatchingExcerpt(text, match, keepOpening: true) : text;
    }

    public static string MatchingExcerpt(string text, TextSelection match, bool keepOpening = false)
    {
        var starts = System.Globalization.StringInfo.ParseCombiningCharacters(text);
        var matchStart = Array.BinarySearch(starts, match.StartUtf16);
        return matchStart <= (keepOpening ? 39 : 8) ? text : "…" + text[starts[matchStart - 8]..];
    }

    public static TextSelection? SourceMatch(Sentence sentence, string query) =>
        SourceMatch(sentence.Text, query, sentence.StartUtf16);

    public static TextSelection? SourceMatch(string source, string query, int startUtf16 = 0) =>
        SourceMatches(source, query, startUtf16).FirstOrDefault();

    public static IEnumerable<TextSelection> PdfSourceMatches(string source, string query, string language,
        int startUtf16 = 0)
    {
        var original = SourceMatches(source, query, startUtf16);
        if (!source.Contains('\n') || source.AsSpan().IndexOfAny('-', '\u00ad', '\u2010') < 0) return original;
        var block = new DocumentBlock("paragraph", 0, 0, 0, source.Length, null, null);
        var projection = new ReaderTextProjection(source, [block], reflowPdf: true, sourceLanguage: language);
        if (projection.Text == source) return original;
        var reflow = SourceMatches(projection.Text, query).Select(match =>
            projection.ToSource(match.StartUtf16, match.LengthUtf16) is { } span
                ? new TextSelection(startUtf16 + span.Start, span.Length, source.Substring(span.Start, span.Length))
                : null).OfType<TextSelection>();
        return original.Concat(reflow).DistinctBy(match => (match.StartUtf16, match.LengthUtf16))
            .OrderBy(match => match.StartUtf16).ThenBy(match => match.LengthUtf16);
    }

    public static IEnumerable<TextSelection> SourceMatches(string source, string query, int startUtf16 = 0)
    {
        var needle = Normalize(query.Trim());
        if (needle.Length == 0) yield break;
        var normalized = Normalize(source);
        var match = normalized.IndexOf(needle, StringComparison.OrdinalIgnoreCase);
        var normalizedOffset = 0;
        var elementStart = 0;
        var elementEnd = 0;
        var previousWhitespace = true;
        var elements = System.Globalization.StringInfo.GetTextElementEnumerator(source);
        while (match >= 0)
        {
            var endOfMatch = match + needle.Length;
            var sourceStart = normalizedOffset > match ? elementStart : -1;
            while (normalizedOffset < endOfMatch && elements.MoveNext())
            {
                var value = elements.GetTextElement();
                var whitespace = string.IsNullOrWhiteSpace(value);
                if (whitespace && previousWhitespace) continue;
                previousWhitespace = whitespace;
                normalizedOffset += whitespace ? 1 : Canonical(value).Length;
                elementStart = elements.ElementIndex;
                elementEnd = elementStart + value.Length;
                if (sourceStart < 0 && normalizedOffset > match) sourceStart = elementStart;
            }
            if (sourceStart < 0 || normalizedOffset < endOfMatch) yield break;
            yield return new TextSelection(startUtf16 + sourceStart, elementEnd - sourceStart, source[sourceStart..elementEnd]);
            match = normalized.IndexOf(needle, endOfMatch, StringComparison.OrdinalIgnoreCase);
        }
    }

    private static string Normalize(string text) =>
        string.Join(" ", Canonical(text).Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));

    private static string Canonical(string text) => text.Normalize().Replace('’', '\'').Replace('ʼ', '\'');
}
