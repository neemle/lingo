using System.Text.RegularExpressions;
using Bounds = Reader.Presentation.PdfOrderedText.WordBounds;

namespace Reader.Presentation;

internal static class PdfTextBlocks
{
    internal readonly record struct Slice(int Start, int End, string Separator = "");
    internal sealed record Edit(int Start, int End, Slice[] Slices);
    private sealed record Word(int Start, int End, Bounds Box);
    private sealed record Line(Word[] Words)
    {
        public Word First => Words[0];
        public Word Last => Words[^1];
    }

    internal static Edit[] Separate(string text, Func<int, int, Bounds?> geometry, double pageWidth)
    {
        if (text.Length == 0 || text.Length > 24000 || pageWidth <= 0) return [];
        var lines = Regex.Matches(text, @"[^\r\n]+").Select(line =>
        {
            var words = Regex.Matches(line.Value, @"[^\s\uFFFC]+")
                .Select(token =>
                {
                    var start = line.Index + token.Index;
                    return geometry(start, token.Length) is { } box
                        ? new Word(start, start + token.Length, box) : null;
                }).OfType<Word>().ToArray();
            return words.Length > 0 ? new Line(words) : null;
        }).ToArray();
        var edits = new List<Edit>();
        for (var index = 0; index < lines.Length; index++)
        {
            if (index + 3 < lines.Length && lines[index] is { } leftTop && lines[index + 1] is { } rightTop &&
                lines[index + 2] is { } leftBottom && lines[index + 3] is { } rightBottom &&
                TryParallelLines(index, leftTop, rightTop, leftBottom, rightBottom) is { } block)
            {
                edits.Add(block.Edit);
                index += block.LineCount - 1;
                continue;
            }

            if (lines[index] is not { } line) continue;
            for (var split = 2; split <= line.Words.Length - 2; split++)
            {
                var left = line.Words[split - 1];
                var right = line.Words[split];
                if (SameBaseline(left.Box, right.Box) && WideGap(left.Box, right.Box) &&
                    text.AsSpan(left.End, right.Start - left.End).Trim().IsEmpty)
                    edits.Add(new(left.End, right.Start, []));
            }
        }
        return edits.ToArray();

        (Edit Edit, int LineCount)? TryParallelLines(int firstLine, Line leftTop, Line rightTop,
            Line leftBottom, Line rightBottom)
        {
            if (leftTop.Words.Length < 2 || leftBottom.Words.Length < 2 || rightBottom.Words.Length < 2 ||
                rightTop.Words.Length < 2 || !Aligned(leftTop.First.Box, leftBottom.First.Box) ||
                !SameBaseline(leftBottom.First.Box, rightBottom.First.Box) ||
                !WideGap(leftBottom.Last.Box, rightBottom.First.Box)) return null;

            var right = rightTop.First;
            Word? middle = null;
            if (!Aligned(right.Box, rightBottom.First.Box) && rightTop.Words.Length >= 3)
            {
                var next = rightTop.Words[1];
                if (right.Box.Right - right.Box.Left <= 2.5 * next.Box.Height &&
                    right.Box.Left > leftTop.Last.Box.Right &&
                    next.Box.Left - right.Box.Right >= 1.25 * next.Box.Height)
                {
                    middle = right;
                    right = next;
                }
            }
            var height = leftTop.First.Box.Height;
            if (!NextLine(leftTop.First.Box, leftBottom.First.Box) ||
                !Aligned(right.Box, rightBottom.First.Box) || !SameBaseline(leftTop.First.Box, right.Box) ||
                !WideGap(leftTop.Last.Box, right.Box)) return null;

            var start = leftTop.First.Start;
            var leftLines = new List<Slice> { new(start, leftTop.Last.End) };
            var rightLines = new List<Slice> { new(right.Start, rightTop.Last.End, "\n\n") };
            var index = firstLine + 2;
            while (true)
            {
                leftLines.Add(new(leftBottom.First.Start, leftBottom.Last.End, "\n"));
                rightLines.Add(new(rightBottom.First.Start, rightBottom.Last.End, "\n"));
                index += 2;
                if (index - firstLine >= 48 ||
                    index + 1 >= lines.Length || lines[index] is not { Words.Length: >= 2 } nextLeft ||
                    lines[index + 1] is not { Words.Length: >= 2 } nextRight ||
                    !Aligned(leftTop.First.Box, nextLeft.First.Box) || !Aligned(right.Box, nextRight.First.Box) ||
                    !NextLine(leftBottom.First.Box, nextLeft.First.Box) ||
                    !SameBaseline(nextLeft.First.Box, nextRight.First.Box) ||
                    !WideGap(nextLeft.Last.Box, nextRight.First.Box)) break;
                leftBottom = nextLeft;
                rightBottom = nextRight;
            }
            var end = rightBottom.Last.End;
            // Source pieces must cover the original tokens exactly, including any center label.
            if (middle is { } label) leftLines.Add(new(label.Start, label.End, "\n\n"));
            leftLines.AddRange(rightLines);
            var cursor = start;
            foreach (var slice in leftLines.OrderBy(slice => slice.Start))
            {
                if (!text.AsSpan(cursor, slice.Start - cursor).Trim().IsEmpty) return null;
                cursor = slice.End;
            }
            return (new(start, end, leftLines.ToArray()), index - firstLine);

            bool NextLine(Bounds upper, Bounds lower)
            {
                var drop = upper.MidY - lower.MidY;
                return drop >= height * 0.6 && drop <= height * 2.2;
            }
        }

        bool WideGap(Bounds left, Bounds right) =>
            right.Left - left.Right >= Math.Max(pageWidth * 0.085, 3.5 * Math.Max(left.Height, right.Height));
    }

    private static bool SameBaseline(Bounds left, Bounds right) =>
        Math.Abs(left.MidY - right.MidY) < 0.55 * Math.Min(left.Height, right.Height);

    private static bool Aligned(Bounds upper, Bounds lower) =>
        Math.Abs(upper.Left - lower.Left) <= Math.Min(upper.Height, lower.Height) * 0.5;
}
