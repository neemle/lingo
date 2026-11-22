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

    private static string Normalize(string text) => text.Normalize().Replace('’', '\'').Replace('ʼ', '\'');
}
