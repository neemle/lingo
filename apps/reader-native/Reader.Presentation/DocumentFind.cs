using Reader.Contracts;

namespace Reader.Presentation;

public static class DocumentFind
{
    public const int PageSize = 30;

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
        var normalized = text.Normalize();
        var match = Normalize(normalized).IndexOf(Normalize(query.Trim()), StringComparison.OrdinalIgnoreCase);
        if (match < 0) return text;

        var starts = System.Globalization.StringInfo.ParseCombiningCharacters(normalized);
        var matchStart = Array.BinarySearch(starts, match);
        if (matchStart < 0) matchStart = Math.Max(0, ~matchStart - 1);
        var start = matchStart >= 40 ? matchStart - 8 : 0;
        return start == 0 ? normalized : "…" + normalized[starts[start]..];
    }

    private static string Normalize(string text) => text.Normalize().Replace('’', '\'').Replace('ʼ', '\'');
}
