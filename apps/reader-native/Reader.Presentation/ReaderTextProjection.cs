using System.Text;
using Reader.Contracts;

namespace Reader.Presentation;

/// <summary>Maps DOCX list markers in the native text view back to canonical UTF-16 offsets.</summary>
public sealed class ReaderTextProjection
{
    private readonly record struct Marker(int SourceStart, int DisplayStart, int Length)
    {
        public int ShiftBefore => DisplayStart - SourceStart;
        public int ShiftAfter => ShiftBefore + Length;
    }

    private readonly int sourceLength;
    private readonly Marker[] markers;

    public ReaderTextProjection(string source, IReadOnlyList<DocumentBlock> blocks, bool reflowPdf = false)
    {
        sourceLength = source.Length;
        if (reflowPdf) source = JoinPdfLines(source, blocks);
        StringBuilder? display = null;
        var insertions = new List<Marker>();
        var previousStart = -1;
        var copied = 0;
        foreach (var block in blocks)
        {
            var start = block.StartUtf16;
            if (block.List?.Marker is not { Length: > 0 } marker || start <= previousStart ||
                start < 0 || start >= source.Length || block.EndUtf16 <= start || block.EndUtf16 > source.Length ||
                char.IsLowSurrogate(source[start]) || marker.Any(char.IsControl))
                continue;

            display ??= new StringBuilder(source.Length);
            display.Append(source.AsSpan(copied, start - copied));
            insertions.Add(new Marker(start, display.Length, marker.Length + 1));
            display.Append(marker).Append('\u00a0');
            copied = start;
            previousStart = start;
        }

        markers = insertions.ToArray();
        if (display is not null) display.Append(source.AsSpan(copied));
        Text = display?.ToString() ?? source;
    }

    private static string JoinPdfLines(string source, IReadOnlyList<DocumentBlock> blocks)
    {
        char[]? reflowed = null;
        var previousEnd = -1;
        foreach (var block in blocks)
        {
            if (block.StartUtf16 < 0 || block.EndUtf16 > source.Length || block.EndUtf16 <= block.StartUtf16) continue;
            if (previousEnd >= 0 && previousEnd < block.StartUtf16)
            {
                var gap = source.AsSpan(previousEnd, block.StartUtf16 - previousEnd);
                if (gap.IndexOfAny('\r', '\n') < 0 && gap.Trim().IsEmpty)
                {
                    reflowed ??= source.ToCharArray();
                    reflowed[previousEnd] = '\n';
                }
            }
            previousEnd = block.EndUtf16;
            for (var index = block.StartUtf16; index < block.EndUtf16; index++)
            {
                if (source[index] is not ('\r' or '\n')) continue;
                reflowed ??= source.ToCharArray();
                reflowed[index] = index == block.StartUtf16 + 1 && char.IsUpper(source[block.StartUtf16]) &&
                    index + 1 < block.EndUtf16 && char.IsUpper(source[index + 1]) ? '\u200b' : ' ';
            }
        }
        return reflowed is null ? source : new string(reflowed);
    }

    public string Text { get; }

    public (int Start, int Length) ToDisplay(int start, int length, bool includeLeadingMarker = false)
    {
        var displayStart = start + SourceShift(start, includeEqual: !includeLeadingMarker);
        if (length == 0) return (displayStart, 0);
        var end = start + length;
        var displayEnd = end + SourceShift(end, includeEqual: false);
        return (displayStart, displayEnd - displayStart);
    }

    public (int Start, int Length)? ToSource(int start, int length)
    {
        if (start < 0 || length <= 0 || start > Text.Length - length) return null;
        var sourceStart = SourceOffset(start);
        var sourceEnd = SourceOffset(start + length);
        return sourceStart < sourceEnd ? (sourceStart, sourceEnd - sourceStart) : null;
    }

    private int SourceShift(int offset, bool includeEqual)
    {
        var index = FindInsertion(offset, display: false, includeEqual);
        return index < 0 ? 0 : markers[index].ShiftAfter;
    }

    private int SourceOffset(int offset)
    {
        var index = FindInsertion(offset, display: true, includeEqual: true);
        if (index < 0) return offset;
        var marker = markers[index];
        return Math.Min(sourceLength, Math.Max(marker.SourceStart, offset - marker.ShiftAfter));
    }

    private int FindInsertion(int offset, bool display, bool includeEqual)
    {
        var left = 0;
        var right = markers.Length;
        while (left < right)
        {
            var middle = left + (right - left) / 2;
            var candidate = display ? markers[middle].DisplayStart : markers[middle].SourceStart;
            if (candidate < offset || includeEqual && candidate == offset)
                left = middle + 1;
            else
                right = middle;
        }
        return left - 1;
    }
}
