using System.Globalization;
using Reader.Contracts;

namespace Reader.Presentation;

public sealed class SourceRanges(string text)
{
    private int[]? boundaries;
    public string Text { get; } = text;
    private int[] Boundaries => boundaries ??= StringInfo.ParseCombiningCharacters(Text);

    public static (int First, int Count)? ResolveTranslationSource(Document document, TranslationAlignment? alignment)
    {
        if ((uint)document.Position >= (uint)document.Sentences.Length) return null;
        if (alignment is null) return (document.Position, 1);

        var ids = alignment.SourceSentenceIds;
        var active = Array.IndexOf(ids, document.Sentences[document.Position].Id);
        if (active < 0 || active > document.Position) return null;
        var first = document.Position - active;
        if (ids.Length > document.Sentences.Length - first) return null;

        for (var index = 0; index < ids.Length; index++)
            if (document.Sentences[first + index].Id != ids[index]) return null;
        return (first, ids.Length);
    }

    public static (string Text, Action<int, int>? Boundary) PrepareSpeech(string source, Action<int, int>? boundary)
    {
        // AVSpeechSynthesizer can report offsets in composed text even when its input is decomposed.
        var spoken = source.Normalize();
        if (boundary is null || spoken == source) return (spoken, boundary);

        var originalStarts = StringInfo.ParseCombiningCharacters(source);
        var spokenStarts = StringInfo.ParseCombiningCharacters(spoken);
        if (originalStarts.Length != spokenStarts.Length) return (spoken, null);

        return (spoken, MapSpeechBoundary(source.Length, spoken.Length, originalStarts, spokenStarts, 0, boundary));
    }

    public static (string Text, Action<int, int>? Boundary) ProjectPronunciation(
        string source, string spoken, int sourceOffset, Action<int, int> boundary)
    {
        if (spoken != source)
        {
            var originalStarts = StringInfo.ParseCombiningCharacters(source);
            var spokenStarts = StringInfo.ParseCombiningCharacters(spoken);
            if (originalStarts.Length == spokenStarts.Length)
            {
                var first = Array.BinarySearch(originalStarts, sourceOffset);
                if (first >= 0)
                {
                    var from = spokenStarts[first];
                    return (spoken[from..],
                        MapSpeechBoundary(source.Length, spoken.Length, originalStarts, spokenStarts, from, boundary));
                }
            }
            else if (TryMapOcrPronunciation(source, spoken, originalStarts, spokenStarts) is { } map)
            {
                var first = CeilingIndex(map.Starts, sourceOffset);
                if (first == spokenStarts.Length) return ("", null);
                var from = spokenStarts[first];
                return (spoken[from..],
                    MapSpeechBoundary(source.Length, spoken.Length, map.Starts, spokenStarts, from, boundary, map.Ends));
            }

            // Keep the cleaned pronunciation even when a source word cannot be mapped exactly.
            return (spoken, null);
        }

        return (source[sourceOffset..], (start, length) => boundary(sourceOffset + start, length));
    }

    private static (int[] Starts, int[] Ends)? TryMapOcrPronunciation(
        string source, string spoken, int[] originalStarts, int[] spokenStarts)
    {
        if (spokenStarts.Length >= originalStarts.Length) return null;

        var starts = new int[spokenStarts.Length];
        var ends = new int[spokenStarts.Length];
        var matched = 0;
        for (var index = 0; index < originalStarts.Length; index++)
        {
            var start = originalStarts[index];
            var end = index + 1 < originalStarts.Length ? originalStarts[index + 1] : source.Length;
            var original = source.AsSpan(start, end - start);
            if (matched < spokenStarts.Length)
            {
                var from = spokenStarts[matched];
                var to = matched + 1 < spokenStarts.Length ? spokenStarts[matched + 1] : spoken.Length;
                var pronounced = spoken.AsSpan(from, to - from);
                // The core may insert a Ukrainian stress mark or blank a classified note number.
                if (original.TrimEnd('\u0301').SequenceEqual(pronounced.TrimEnd('\u0301')) ||
                    char.IsWhiteSpace(pronounced[0]) &&
                    (char.IsWhiteSpace(original[0]) || char.IsNumber(original[0])))
                {
                    starts[matched] = start;
                    ends[matched++] = end;
                    continue;
                }
            }

            if (char.IsWhiteSpace(original[0]) || char.IsNumber(original[0])) continue;
            if (original is "-" or "\u00ad" or "\u2010")
            {
                var tail = source.AsSpan(end);
                if (tail[..(tail.Length - tail.TrimStart().Length)].Contains('\n')) continue;
            }
            return null;
        }
        return matched == spokenStarts.Length ? (starts, ends) : null;
    }

    private static Action<int, int> MapSpeechBoundary(int originalLength, int spokenLength,
        int[] originalStarts, int[] spokenStarts, int spokenOffset, Action<int, int> boundary,
        int[]? originalEnds = null) =>
        (start, length) =>
        {
            if (start < 0 || length <= 0 || start > spokenLength - spokenOffset - length) return;
            var first = FloorIndex(spokenStarts, spokenOffset + start);
            var after = CeilingIndex(spokenStarts, spokenOffset + start + length);
            var originalStart = originalStarts[first];
            var originalEnd = originalEnds is not null ? originalEnds[after - 1]
                : after < originalStarts.Length ? originalStarts[after] : originalLength;
            boundary(originalStart, originalEnd - originalStart);
        };

    public static TextSelection? TrimFootnoteReference(TextSelection selection,
        IReadOnlyList<DocumentBlock> blocks)
    {
        var end = selection.StartUtf16 + selection.LengthUtf16;
        foreach (var block in blocks)
        {
            if (block.StartUtf16 >= end) break;
            if (block.EndUtf16 <= selection.StartUtf16) continue;
            foreach (var marker in block.FootnoteReferences)
            {
                if (marker.StartUtf16 >= end || marker.EndUtf16 < end) continue;
                if (marker.StartUtf16 <= selection.StartUtf16) return null;
                var length = marker.StartUtf16 - selection.StartUtf16;
                return selection with { LengthUtf16 = length, Text = selection.Text[..length], RejoinedWord = null };
            }
        }
        return selection;
    }

    public static string MaskFootnoteReferences(string text, int start, IReadOnlyList<DocumentBlock> blocks)
    {
        var end = start + text.Length;
        char[]? masked = null;
        foreach (var block in blocks)
        {
            if (block.StartUtf16 >= end) break;
            if (block.EndUtf16 <= start) continue;
            foreach (var marker in block.FootnoteReferences)
            {
                if (marker.StartUtf16 < start || marker.EndUtf16 > end || marker.StartUtf16 >= marker.EndUtf16) continue;
                masked ??= text.ToCharArray();
                masked.AsSpan(marker.StartUtf16 - start, marker.EndUtf16 - marker.StartUtf16).Fill(' ');
            }
        }
        return masked is null ? text : new string(masked);
    }

    public static TextSelection? Selection(string source, int start, int length) =>
        new SourceRanges(source).Selection(start, length);

    public TextSelection? Selection(int start, int length)
    {
        if (start < 0 || length <= 0 || start > Text.Length - length) return null;
        var starts = Boundaries;
        var after = CeilingIndex(starts, start + length);
        var alignedStart = starts[FloorIndex(starts, start)];
        var alignedEnd = after < starts.Length ? starts[after] : Text.Length;
        return new TextSelection(alignedStart, alignedEnd - alignedStart, Text[alignedStart..alignedEnd]);
    }

    public static TextSelection? WordAt(string source, int offset) => new SourceRanges(source).WordAt(offset);

    public TextSelection? WordAt(int offset)
    {
        if (offset < 0 || offset >= Text.Length) return null;
        var starts = Boundaries;
        var index = FloorIndex(starts, offset);
        if (!IsWord(Text, starts[index])) return null;
        var first = index;
        var last = index;
        while (first > 0 && IsWord(Text, starts[first - 1])) first--;
        while (last + 1 < starts.Length && IsWord(Text, starts[last + 1])) last++;
        var start = starts[first];
        var end = last + 1 < starts.Length ? starts[last + 1] : Text.Length;
        return new TextSelection(start, end - start, Text[start..end]);
    }

    private static int FloorIndex(int[] starts, int offset)
    {
        var index = Array.BinarySearch(starts, offset);
        return index >= 0 ? index : ~index - 1;
    }

    private static int CeilingIndex(int[] starts, int offset)
    {
        var index = Array.BinarySearch(starts, offset);
        return index >= 0 ? index : ~index;
    }

    private static bool IsWord(string source, int offset) =>
        char.IsLetterOrDigit(source, offset) || source[offset] is '\'' or '’';
}
