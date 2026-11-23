using Reader.Contracts;

namespace Reader.Presentation;

public static class DocumentFind
{
    public const int PageSize = 30;

    public static bool Contains(string text, string query)
    {
        var needle = Normalize(query.Trim());
        return needle.Length > 0 && Normalize(text).Contains(needle, StringComparison.OrdinalIgnoreCase);
    }

    public static (int[] Positions, int? NextStart) FindPage(Sentence[] sentences, string query,
        int start, CancellationToken cancellationToken)
    {
        var needle = Normalize(query.Trim());
        if (needle.Length == 0) return ([], null);

        var positions = new List<int>(PageSize);
        for (var position = Math.Max(0, start); position < sentences.Length; position++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var text = Normalize(sentences[position].Text);
            if (!text.Contains(needle, StringComparison.OrdinalIgnoreCase)) continue;
            if (positions.Count == PageSize) return (positions.ToArray(), position);
            positions.Add(position);
        }
        return (positions.ToArray(), null);
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
