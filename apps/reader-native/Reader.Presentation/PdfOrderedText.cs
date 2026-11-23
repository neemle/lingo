using System.Text;
using System.Text.RegularExpressions;
using Reader.Contracts;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Presentation;

public sealed class PdfOrderedText
{
    public readonly record struct WordBounds(double Left, double Right, double MidY, double Height);
    private readonly record struct SourceSlice(int OutputStart, int SourceStart, int Length);
    private readonly record struct Edit(int Start, int End, int? MovedWord);
    private static readonly Regex IsolatedAccent = new(@"(?m)^[ \t]*(à)[ \t\r]*$");
    private static readonly Regex Tokens = new(@"\S+");
    private static readonly Regex TextTokens = new(@"[^\s\uFFFC]+");
    private readonly SourceSlice[] slices;

    public string Text { get; }
    public PdfTextOrderEdit[] Corrections { get; } = [];

    public PdfOrderedText(string text)
    {
        Text = text;
        slices = [];
    }

    private PdfOrderedText(string text, SourceSlice[] slices, PdfTextOrderEdit[] corrections)
    {
        Text = text;
        this.slices = slices;
        Corrections = corrections;
    }

    public int? OriginalOffset(int start, int length)
    {
        if (slices.Length == 0) return start;
        foreach (var slice in slices)
            if (start >= slice.OutputStart && start - slice.OutputStart <= slice.Length - length)
                return slice.SourceStart + start - slice.OutputStart;
        return null;
    }

    public PdfOrderedText SeparateBlocks(Func<int, int, WordBounds?> bounds, double pageWidth)
    {
        if (Text.Length == 0 || Text.Length > 24000 || pageWidth <= 0) return this;
        var words = new List<PdfLayoutWord>();
        foreach (Match token in TextTokens.Matches(Text))
            if (OriginalOffset(token.Index, token.Length) is { } original && bounds(original, token.Length) is { } box)
                words.Add(new(token.Index, token.Index + token.Length, box.Left, box.Right, -box.MidY, box.Height));
        if (words.Count == 0) return this;
        var edits = Client.SeparatePdfBlocks(Text, words.ToArray(), pageWidth);
        if (edits.Length == 0) return this;
        var output = new StringBuilder(Text.Length + edits.Length);
        var sources = new List<SourceSlice>();
        var changes = new List<PdfTextOrderEdit>();
        var cursor = 0;
        foreach (var edit in edits)
        {
            AppendSource(cursor, edit.Start);
            var start = output.Length;
            if (edit.Slices.Length == 0) output.Append("\n\n");
            foreach (var slice in edit.Slices)
            {
                output.Append(slice.Separator);
                AppendSource(slice.Start, slice.End);
            }
            changes.Add(new(edit.Start, edit.End, output.ToString(start, output.Length - start)));
            cursor = edit.End;
        }
        AppendSource(cursor, Text.Length);
        return new(output.ToString(), sources.ToArray(), changes.ToArray());

        void AppendSource(int start, int end)
        {
            if (end <= start) return;
            if (slices.Length == 0) sources.Add(new(output.Length, start, end - start));
            else
            {
                foreach (var slice in slices)
                {
                    var left = Math.Max(start, slice.OutputStart);
                    var right = Math.Min(end, slice.OutputStart + slice.Length);
                    if (left < right)
                        sources.Add(new(output.Length + left - start, slice.SourceStart + left - slice.OutputStart,
                            right - left));
                }
            }
            output.Append(Text, start, end - start);
        }
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
            corrections.Add(new(leftEnd, lastEdit, " à " + text[words[insertion].Index..previousEnd]));
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
        return new(output.ToString(), sources.ToArray(), corrections.ToArray());

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
