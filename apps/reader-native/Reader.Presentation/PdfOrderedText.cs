using System.Text;
using System.Text.RegularExpressions;

namespace Reader.Presentation;

public sealed class PdfOrderedText
{
    public readonly record struct WordBounds(double Left, double Right, double MidY, double Height);
    private readonly record struct SourceSlice(int OutputStart, int SourceStart, int Length);
    private readonly record struct Edit(int Start, int End, int? MovedWord);
    private static readonly Regex IsolatedAccent = new(@"(?m)^[ \t]*(à)[ \t\r]*$");
    private static readonly Regex Tokens = new(@"\S+");
    private readonly SourceSlice[] slices;

    public string Text { get; }

    public PdfOrderedText(string text)
    {
        Text = text;
        slices = [];
    }

    private PdfOrderedText(string text, SourceSlice[] slices)
    {
        Text = text;
        this.slices = slices;
    }

    public int? OriginalOffset(int start, int length)
    {
        if (slices.Length == 0) return start;
        foreach (var slice in slices)
            if (start >= slice.OutputStart && start - slice.OutputStart <= slice.Length - length)
                return slice.SourceStart + start - slice.OutputStart;
        return null;
    }

    public static PdfOrderedText Repair(string text, Func<int, int, WordBounds?> bounds)
    {
        var accents = IsolatedAccent.Matches(text);
        if (accents.Count == 0) return new(text);
        var words = Tokens.Matches(text).ToArray();
        var edits = new List<Edit>();
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
            edits.Add(new(previous.Index + previous.Length, lastEdit, null));
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
        return new(output.ToString(), sources.ToArray());

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
