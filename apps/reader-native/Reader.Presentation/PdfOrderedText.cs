using System.Text;
using System.Text.RegularExpressions;
using Reader.Contracts;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Presentation;

public sealed class PdfOrderedText
{
    public readonly record struct WordBounds(double Left, double Right, double MidY, double Height, int? Rotation = null);
    private readonly record struct SourceSlice(int OutputStart, int SourceStart, int Length);
    private readonly record struct Edit(int Start, int End, int? MovedWord);
    private static readonly Regex IsolatedAccent = new(@"(?m)^[ \t]*(à)[ \t\r]*$");
    private static readonly Regex Tokens = new(@"\S+");
    private static readonly Regex TextTokens = new(@"[^\s\uFFFC]+");
    private static readonly Regex AttachedDigits = new(@"(?<=\p{L}[.!?…]?[)\]»”]?)[0-9]{1,3}(?![\p{L}\p{N}])");
    private readonly SourceSlice[] slices;
    private readonly string originalText;

    public string Text { get; }
    public PdfTextOrderEdit[] Corrections { get; } = [];

    public PdfOrderedText(string text)
    {
        Text = text;
        originalText = text;
        slices = [];
    }

    private PdfOrderedText(string text, string originalText, SourceSlice[] slices, PdfTextOrderEdit[] corrections)
    {
        Text = text;
        this.originalText = originalText;
        this.slices = slices;
        Corrections = corrections;
    }

    public int? OriginalOffset(int start, int length) => OriginalRange(start, length)?.Start;

    public PdfOrderedText MarkRaisedNoteReferences(Func<int, int, bool> smallerFont)
    {
        const string superscript = "⁰¹²³⁴⁵⁶⁷⁸⁹";
        char[]? output = null;
        var changes = new List<PdfTextOrderEdit>();
        foreach (Match digits in AttachedDigits.Matches(Text))
        {
            if (OriginalRange(digits.Index, digits.Length) is not { } source ||
                OriginalOffset(digits.Index - 1, 1) is not int preceding ||
                source.Start != preceding + 1 || !smallerFont(source.Start, source.Length)) continue;
            output ??= Text.ToCharArray();
            for (var index = digits.Index; index < digits.Index + digits.Length; index++)
                output[index] = superscript[output[index] - '0'];
            changes.Add(new(digits.Index, digits.Index + digits.Length, new string(output, digits.Index, digits.Length)));
        }
        return output is null ? this : new(new string(output), originalText, slices, changes.ToArray());
    }

    public (int Start, int Length)? OriginalRange(int start, int length)
    {
        if (slices.Length == 0) return (start, length);
        var cursor = start;
        var end = start + length;
        var originalStart = -1;
        var originalEnd = 0;
        foreach (var slice in slices)
        {
            if (slice.OutputStart + slice.Length <= cursor) continue;
            if (slice.OutputStart > cursor) break;
            var next = slice.SourceStart + cursor - slice.OutputStart;
            if (originalStart < 0) originalStart = next;
            else if (next < originalEnd || originalText.AsSpan(originalEnd, next - originalEnd)
                .IndexOfAnyExcept(" \t\u00a0\u202f") >= 0) break;
            var count = Math.Min(end, slice.OutputStart + slice.Length) - cursor;
            cursor += count;
            originalEnd = next + count;
            if (cursor == end) return (originalStart, originalEnd - originalStart);
        }
        return null;
    }

    public PdfOrderedText JoinLigatures(Func<int, int, WordBounds?> bounds)
    {
        if (Text.Length == 0 || Text.Length > 24000) return this;
        var words = TextTokens.Matches(Text);
        var candidates = new SortedDictionary<int, PdfLayoutWord>();
        for (var index = 0; index + 1 < words.Count; index++)
        {
            var left = words[index];
            var right = words[index + 1];
            if ((left.ValueSpan.EndsWith("fi", StringComparison.Ordinal) ||
                 left.ValueSpan.EndsWith("fl", StringComparison.Ordinal)) &&
                char.IsLower(right.ValueSpan[0]))
            {
                Add(left);
                Add(right);
            }
        }
        if (candidates.Count < 2) return this;
        var joins = Client.FindPdfLigatureJoins(Text, candidates.Values.ToArray());
        if (joins.Length == 0) return this;
        var output = new StringBuilder(Text.Length);
        var sources = new List<SourceSlice>();
        var changes = new List<PdfTextOrderEdit>(joins.Length);
        var cursor = 0;
        foreach (var join in joins)
        {
            AppendSource(output, sources, cursor, join.Start);
            changes.Add(new(join.Start, join.End, ""));
            cursor = join.End;
        }
        AppendSource(output, sources, cursor, Text.Length);
        return new(output.ToString(), originalText, sources.ToArray(), changes.ToArray());

        void Add(Match token)
        {
            if (!candidates.ContainsKey(token.Index) &&
                OriginalRange(token.Index, token.Length) is { } original && bounds(original.Start, original.Length) is { } box)
                candidates.Add(token.Index, new(token.Index, token.Index + token.Length,
                    box.Left, box.Right, -box.MidY, box.Height));
        }
    }

    public PdfOrderedText SeparateBlocks(Func<int, int, WordBounds?> bounds, double pageWidth)
    {
        if (Text.Length == 0 || Text.Length > 24000 || pageWidth <= 0) return this;
        var words = new List<PdfLayoutWord>();
        foreach (Match token in TextTokens.Matches(Text))
            if (OriginalRange(token.Index, token.Length) is { } original && bounds(original.Start, original.Length) is { } box)
                words.Add(new(token.Index, token.Index + token.Length, box.Left, box.Right, -box.MidY, box.Height,
                    box.Rotation));
        if (words.Count == 0) return this;
        var edits = Client.SeparatePdfBlocks(Text, words.ToArray(), pageWidth);
        if (edits.Length == 0) return this;
        var output = new StringBuilder(Text.Length + edits.Length);
        var sources = new List<SourceSlice>();
        var changes = new List<PdfTextOrderEdit>();
        var cursor = 0;
        foreach (var edit in edits)
        {
            AppendSource(output, sources, cursor, edit.Start);
            var start = output.Length;
            var preserved = new List<PdfTextOrderSlice>(edit.Slices.Length);
            if (edit.Slices.Length == 0) output.Append("\n\n");
            foreach (var slice in edit.Slices)
            {
                output.Append(slice.Separator);
                preserved.Add(new(slice.Start - edit.Start, slice.End - edit.Start, output.Length - start));
                AppendSource(output, sources, slice.Start, slice.End);
            }
            changes.Add(new(edit.Start, edit.End, output.ToString(start, output.Length - start), preserved.ToArray()));
            cursor = edit.End;
        }
        AppendSource(output, sources, cursor, Text.Length);
        return new(output.ToString(), originalText, sources.ToArray(), changes.ToArray());
    }

    public static PdfTextOrderEdit[] MissingBlockBreaks(string stored, string current)
    {
        if (stored == current || stored.Length > 24000 || current.Length > 24000) return [];
        var oldTokens = Tokens.Matches(stored);
        var newTokens = Tokens.Matches(current);
        if (oldTokens.Count != newTokens.Count) return [];

        var corrections = new List<PdfTextOrderEdit>();
        var oldEnd = 0;
        var newEnd = 0;
        for (var index = 0; index < oldTokens.Count; index++)
        {
            var oldToken = oldTokens[index];
            var newToken = newTokens[index];
            if (!oldToken.ValueSpan.SequenceEqual(newToken.ValueSpan)) return [];
            if (index > 0 && current.AsSpan(newEnd, newToken.Index - newEnd).SequenceEqual("\n\n"))
            {
                var gap = stored.AsSpan(oldEnd, oldToken.Index - oldEnd);
                if (gap.Length > 0 && !HasMultipleLineBreaks(gap))
                    corrections.Add(new(oldEnd, oldToken.Index, "\n\n"));
            }
            oldEnd = oldToken.Index + oldToken.Length;
            newEnd = newToken.Index + newToken.Length;
        }
        return corrections.ToArray();
    }

    private static bool HasMultipleLineBreaks(ReadOnlySpan<char> gap)
    {
        var first = gap.IndexOfAny('\r', '\n');
        if (first < 0) return false;
        var next = first + 1;
        if (gap[first] == '\r' && next < gap.Length && gap[next] == '\n') next++;
        return gap[next..].IndexOfAny('\r', '\n') >= 0;
    }

    private void AppendSource(StringBuilder output, List<SourceSlice> sources, int start, int end)
    {
        if (end <= start) return;
        if (slices.Length == 0) sources.Add(new(output.Length, start, end - start));
        else
            foreach (var slice in slices)
            {
                var left = Math.Max(start, slice.OutputStart);
                var right = Math.Min(end, slice.OutputStart + slice.Length);
                if (left < right)
                    sources.Add(new(output.Length + left - start, slice.SourceStart + left - slice.OutputStart,
                        right - left));
            }
        output.Append(Text, start, end - start);
    }

    public static PdfOrderedText Repair(string text, Func<int, int, WordBounds?> bounds)
    {
        var accents = IsolatedAccent.Matches(text);
        if (accents.Count == 0) return new(text);
        var words = Tokens.Matches(text).ToArray();
        var edits = new List<Edit>();
        var corrections = new List<PdfTextOrderEdit>();
        var lastEdit = 0;
        var index = 0;

        foreach (Match accent in accents)
        {
            var letter = accent.Groups[1];
            while (index < words.Length && words[index].Index < letter.Index) index++;
            if (index < 2 || index == words.Length || words[index].Index != letter.Index ||
                bounds(letter.Index, letter.Length) is not { } position) continue;
            var insertion = index;
            while (insertion > 0 && index - insertion < 12 &&
                bounds(words[insertion - 1].Index, words[insertion - 1].Length) is { } prior &&
                SameLine(position, prior) && prior.Left >= position.Right)
                insertion--;
            if (insertion == index || insertion == 0) continue;
            var left = words[insertion - 1];
            var leftEnd = left.Index + left.Length;
            if (leftEnd < lastEdit || bounds(left.Index, left.Length) is not { } before ||
                bounds(words[insertion].Index, words[insertion].Length) is not { } after ||
                !SameLine(position, before) || before.Right > position.Left ||
                position.Left - before.Right > position.Height || after.Left - position.Right > position.Height)
                continue;

            edits.Add(new(leftEnd, words[insertion].Index, letter.Index));
            var previous = words[index - 1];
            lastEdit = letter.Index + letter.Length;
            var previousEnd = previous.Index + previous.Length;
            edits.Add(new(previousEnd, lastEdit, null));
            var movedStart = words[insertion].Index;
            corrections.Add(new(leftEnd, lastEdit, " à " + text[movedStart..previousEnd],
                [new(movedStart - leftEnd, previousEnd - leftEnd, 3),
                 new(letter.Index - leftEnd, lastEdit - leftEnd, 1)]));
        }

        if (edits.Count == 0) return new(text);
        var output = new StringBuilder(text.Length);
        var sources = new List<SourceSlice>();
        var cursor = 0;
        foreach (var edit in edits)
        {
            AppendSource(cursor, edit.Start - cursor);
            if (edit.MovedWord is int source)
            {
                output.Append(' ');
                AppendSource(source, 1);
                output.Append(' ');
            }
            cursor = edit.End;
        }
        AppendSource(cursor, text.Length - cursor);
        return new(output.ToString(), text, sources.ToArray(), corrections.ToArray());

        void AppendSource(int start, int length)
        {
            if (length <= 0) return;
            sources.Add(new(output.Length, start, length));
            output.Append(text, start, length);
        }
    }

    private static bool SameLine(WordBounds a, WordBounds b) =>
        a.Height > 0 && b.Height > 0 && Math.Abs(a.MidY - b.MidY) < Math.Min(a.Height, b.Height) / 4;
}
