using System.Globalization;

namespace Reader.Presentation;

public sealed class SourceRanges(string text)
{
    private int[]? boundaries;
    public string Text { get; } = text;
    private int[] Boundaries => boundaries ??= StringInfo.ParseCombiningCharacters(Text);

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

    public static (string Text, Action<int, int> Boundary) ProjectPronunciation(
        string source, string spoken, int sourceOffset, Action<int, int> boundary)
    {
        if (spoken != source)
        {
            // The core only inserts accents. Each grapheme still corresponds to the same original grapheme.
            var originalStarts = StringInfo.ParseCombiningCharacters(source);
            var spokenStarts = StringInfo.ParseCombiningCharacters(spoken);
            var first = Array.BinarySearch(originalStarts, sourceOffset);
            if (first >= 0 && originalStarts.Length == spokenStarts.Length)
            {
                var from = spokenStarts[first];
                return (spoken[from..],
                    MapSpeechBoundary(source.Length, spoken.Length, originalStarts, spokenStarts, from, boundary));
            }
        }

        return (source[sourceOffset..], (start, length) => boundary(sourceOffset + start, length));
    }

    private static Action<int, int> MapSpeechBoundary(int originalLength, int spokenLength,
        int[] originalStarts, int[] spokenStarts, int spokenOffset, Action<int, int> boundary) =>
        (start, length) =>
        {
            if (start < 0 || length <= 0 || start > spokenLength - spokenOffset - length) return;
            var first = FloorIndex(spokenStarts, spokenOffset + start);
            var after = CeilingIndex(spokenStarts, spokenOffset + start + length);
            var originalStart = originalStarts[first];
            var originalEnd = after < originalStarts.Length ? originalStarts[after] : originalLength;
            boundary(originalStart, originalEnd - originalStart);
        };

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
