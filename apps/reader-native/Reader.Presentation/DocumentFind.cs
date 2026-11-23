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
        if (MatchRange(text, query) is not { } match) return text;

        var starts = System.Globalization.StringInfo.ParseCombiningCharacters(text);
        var matchStart = Array.BinarySearch(starts, match.Start);
        return matchStart < 40 ? text : "…" + text[starts[matchStart - 8]..];
    }

    public static TextSelection? SourceMatch(Sentence sentence, string query) =>
        SourceMatch(sentence.Text, query, sentence.StartUtf16);

    public static TextSelection? SourceMatch(string source, string query, int startUtf16 = 0) =>
        MatchRange(source, query) is { } range
            ? new TextSelection(startUtf16 + range.Start, range.End - range.Start, source[range.Start..range.End])
            : null;

    private static (int Start, int End)? MatchRange(string source, string query)
    {
        var needle = Normalize(query.Trim());
        if (needle.Length == 0) return null;
        var match = Normalize(source).IndexOf(needle, StringComparison.OrdinalIgnoreCase);
        if (match < 0) return null;
        var endOfMatch = match + needle.Length;
        var normalizedOffset = 0;
        var sourceStart = -1;
        var previousWhitespace = true;
        var elements = System.Globalization.StringInfo.GetTextElementEnumerator(source);
        while (elements.MoveNext())
        {
            var value = elements.GetTextElement();
            var whitespace = string.IsNullOrWhiteSpace(value);
            if (whitespace && previousWhitespace) continue;
            previousWhitespace = whitespace;
            normalizedOffset += whitespace ? 1 : Canonical(value).Length;
            if (sourceStart < 0 && normalizedOffset > match) sourceStart = elements.ElementIndex;
            if (normalizedOffset >= endOfMatch)
                return (sourceStart, elements.ElementIndex + value.Length);
        }
        return null;
    }

    private static string Normalize(string text) =>
        string.Join(" ", Canonical(text).Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));

    private static string Canonical(string text) => text.Normalize().Replace('’', '\'').Replace('ʼ', '\'');
}
