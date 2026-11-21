using System.Globalization;

namespace Reader.Presentation;

public static class SourceRanges
{
    public static TextSelection? Selection(string source, int start, int length)
    {
        if (start < 0 || length <= 0 || start > source.Length - length) return null;
        var boundaries = StringInfo.ParseCombiningCharacters(source);
        var end = start + length;
        start = boundaries.LastOrDefault(index => index <= start);
        end = boundaries.FirstOrDefault(index => index >= end, source.Length);
        return new TextSelection(start, end - start, source[start..end]);
    }

    public static TextSelection? WordAt(string source, int offset)
    {
        if (offset < 0 || offset >= source.Length) return null;
        var boundaries = StringInfo.ParseCombiningCharacters(source);
        var index = Array.FindLastIndex(boundaries, item => item <= offset);
        if (!IsWord(source, boundaries[index])) return null;
        var first = index;
        var last = index;
        while (first > 0 && IsWord(source, boundaries[first - 1])) first--;
        while (last + 1 < boundaries.Length && IsWord(source, boundaries[last + 1])) last++;
        var end = last + 1 < boundaries.Length ? boundaries[last + 1] : source.Length;
        return Selection(source, boundaries[first], end - boundaries[first]);
    }

    private static bool IsWord(string source, int offset) =>
        char.IsLetterOrDigit(source, offset) || source[offset] is '\'' or '’';
}
