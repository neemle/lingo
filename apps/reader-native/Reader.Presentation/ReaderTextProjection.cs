using System.Text;
using Reader.Contracts;

namespace Reader.Presentation;

/// <summary>Maps native reading text back to canonical UTF-16 offsets.</summary>
public sealed class ReaderTextProjection
{
    private readonly record struct Marker(int SourceStart, int DisplayStart, int Length)
    {
        public int ShiftBefore => DisplayStart - SourceStart;
        public int ShiftAfter => ShiftBefore + Length;
    }
    private readonly record struct Removal(int SourceStart, int DisplayStart, int Length)
    {
        public int ShiftAfter => SourceStart - DisplayStart + Length;
    }

    private readonly int sourceLength;
    private readonly Marker[] markers;
    private readonly Removal[] removals = [];

    public ReaderTextProjection(string source, IReadOnlyList<DocumentBlock> blocks, bool reflowPdf = false,
        string? sourceLanguage = null)
    {
        sourceLength = source.Length;
        if (reflowPdf)
        {
            (Text, removals) = JoinPdfLines(source, blocks, sourceLanguage);
            markers = [];
            return;
        }
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

    private static (string Text, Removal[] Removals) JoinPdfLines(
        string source, IReadOnlyList<DocumentBlock> blocks, string? language)
    {
        char[]? reflowed = null;
        var removed = new List<Removal>();
        var deleted = 0;
        var previousEnd = -1;
        foreach (var block in blocks)
        {
            if (block.StartUtf16 < previousEnd || block.StartUtf16 < 0 || block.EndUtf16 > source.Length ||
                block.EndUtf16 <= block.StartUtf16) continue;
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
            foreach (var marker in block.FootnoteReferences)
            {
                if (marker.StartUtf16 < block.StartUtf16 || marker.EndUtf16 > block.EndUtf16 ||
                    marker.StartUtf16 >= marker.EndUtf16) continue;
                const string superscript = "⁰¹²³⁴⁵⁶⁷⁸⁹";
                for (var offset = marker.StartUtf16; offset < marker.EndUtf16; offset++)
                {
                    var digit = source[offset];
                    if (digit is < '0' or > '9') continue;
                    reflowed ??= source.ToCharArray();
                    reflowed[offset] = superscript[digit - '0'];
                }
            }
            for (var index = block.StartUtf16; index < block.EndUtf16; index++)
            {
                if (source[index] is not ('\r' or '\n')) continue;
                if (source[index] == '\n' && WordWrap(source, block, index, language) is { } wrap)
                {
                    var length = wrap.End - wrap.Start;
                    removed.Add(new Removal(wrap.Start, wrap.Start - deleted, length));
                    deleted += length;
                    index = wrap.End - 1;
                    continue;
                }
                reflowed ??= source.ToCharArray();
                reflowed[index] = index == block.StartUtf16 + 1 && char.IsUpper(source[block.StartUtf16]) &&
                    index + 1 < block.EndUtf16 && char.IsUpper(source[index + 1]) ? '\u200b' : ' ';
            }
        }
        if (removed.Count == 0) return (reflowed is null ? source : new string(reflowed), []);
        var text = reflowed is null ? source.AsSpan() : reflowed.AsSpan();
        var display = new StringBuilder(source.Length - deleted);
        var start = 0;
        foreach (var removal in removed)
        {
            display.Append(text[start..removal.SourceStart]);
            start = removal.SourceStart + removal.Length;
        }
        display.Append(text[start..]);
        return (display.ToString(), removed.ToArray());
    }

    private static (int Start, int End)? WordWrap(string source, DocumentBlock block, int lineBreak, string? language)
    {
        var dash = lineBreak - 1;
        while (dash > block.StartUtf16 && source[dash] is ' ' or '\t' or '\r' or '\u00a0') dash--;
        var next = lineBreak + 1;
        while (next < block.EndUtf16 && source[next] is ' ' or '\t' or '\u00a0') next++;
        if (dash <= block.StartUtf16 || next >= block.EndUtf16 ||
            source[dash] is not ('-' or '\u00ad' or '\u2010') ||
            !char.IsLetter(source[dash - 1]) || !char.IsLower(source[next])) return null;

        var prefixStart = dash;
        while (prefixStart > block.StartUtf16 &&
            (char.IsLetter(source[prefixStart - 1]) || source[prefixStart - 1] is '-' or '\u2010')) prefixStart--;
        while (prefixStart < dash && source[prefixStart] is '-' or '\u2010') prefixStart++;
        var keepHyphen = source.AsSpan(prefixStart, dash - prefixStart).IndexOfAny('-', '\u2010') >= 0;
        if (!keepHyphen && language == "fr")
        {
            var end = next;
            while (end < block.EndUtf16 && char.IsLetter(source[end])) end++;
            keepHyphen = source.AsSpan(next, end - next) switch
            {
                "il" or "ils" or "elle" or "elles" or "on" or "tu" or "vous" or "nous" or "je" => true,
                "ce" => IsFrenchInversionVerb(source, block.StartUtf16, dash),
                _ => false,
            };
        }
        return (dash + (keepHyphen ? 1 : 0), next);
    }

    private static bool IsFrenchInversionVerb(string source, int start, int end)
    {
        var word = end;
        while (word > start && char.IsLetter(source[word - 1])) word--;
        return source[word..end].ToLowerInvariant() is "est" or "était" or "sera" or "serait" or "fut" or
            "fût" or "sont" or "étaient";
    }

    public string Text { get; }

    public (int Start, int Length) ToDisplay(int start, int length, bool includeLeadingMarker = false)
    {
        if (removals.Length > 0)
        {
            var first = ReflowDisplayOffset(start);
            return (first, ReflowDisplayOffset(start + length) - first);
        }
        var displayStart = start + SourceShift(start, includeEqual: !includeLeadingMarker);
        if (length == 0) return (displayStart, 0);
        var end = start + length;
        var displayEnd = end + SourceShift(end, includeEqual: false);
        return (displayStart, displayEnd - displayStart);
    }

    public (int Start, int Length)? ToSource(int start, int length)
    {
        if (start < 0 || length <= 0 || start > Text.Length - length) return null;
        var sourceStart = removals.Length == 0 ? SourceOffset(start) : ReflowSourceOffset(start, startsRange: true);
        var sourceEnd = removals.Length == 0 ? SourceOffset(start + length) :
            ReflowSourceOffset(start + length, startsRange: false);
        return sourceStart < sourceEnd ? (sourceStart, sourceEnd - sourceStart) : null;
    }

    public string? RejoinedWord(int start, int length)
    {
        if (start < 0 || length <= 0 || start > Text.Length - length) return null;
        var removal = FindRemoval(start, display: true, includeEqual: true) + 1;
        if (removal >= removals.Length || removals[removal].DisplayStart >= start + length) return null;
        var text = Text.AsSpan(start, length);
        foreach (var ch in text)
            if (!char.IsLetter(ch) && ch is not ('-' or '\u2010')) return null;
        return text.ToString();
    }

    private int ReflowDisplayOffset(int source)
    {
        var index = FindRemoval(source, display: false, includeEqual: true);
        return index < 0 ? source : Math.Max(removals[index].DisplayStart, source - removals[index].ShiftAfter);
    }

    private int ReflowSourceOffset(int display, bool startsRange)
    {
        var index = FindRemoval(display, display: true, includeEqual: startsRange);
        return index < 0 ? display : display + removals[index].ShiftAfter;
    }

    private int FindRemoval(int offset, bool display, bool includeEqual)
    {
        var left = 0;
        var right = removals.Length;
        while (left < right)
        {
            var middle = left + (right - left) / 2;
            var position = display ? removals[middle].DisplayStart : removals[middle].SourceStart;
            if (position < offset || includeEqual && position == offset) left = middle + 1;
            else right = middle;
        }
        return left - 1;
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
