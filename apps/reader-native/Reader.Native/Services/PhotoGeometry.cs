using Reader.Contracts;

namespace Reader.Native.Services;

internal sealed record PhotoPreview(byte[] Bytes, int Width, int Height);

internal static class PhotoGeometry
{
    internal static OcrLine? Line(string text, double left, double top, double right, double bottom,
        double? confidence)
    {
        left = Math.Clamp(left, 0, 1);
        top = Math.Clamp(top, 0, 1);
        right = Math.Clamp(right, 0, 1);
        bottom = Math.Clamp(bottom, 0, 1);
        if (!(right > left && bottom > top)) return null;
        text = string.Join(' ', text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
        return text.Length == 0 ? null : new OcrLine(text, left, top, right - left, bottom - top, confidence);
    }

    internal static OcrWord? Word(string text, double left, double top, double right, double bottom,
        int? rawStartUtf16)
    {
        left = Math.Clamp(left, 0, 1);
        top = Math.Clamp(top, 0, 1);
        right = Math.Clamp(right, 0, 1);
        bottom = Math.Clamp(bottom, 0, 1);
        return right > left && bottom > top && !string.IsNullOrWhiteSpace(text)
            ? new OcrWord(text, left, top, right - left, bottom - top, rawStartUtf16) : null;
    }

    internal static int? MatchWord(string normalized, string word, ref int cursor)
    {
        if (word.Length == 0) return null;
        var start = normalized.IndexOf(word, cursor, StringComparison.Ordinal);
        if (start < 0) return null;
        for (var index = cursor; index < start; index++)
            if (!char.IsWhiteSpace(normalized[index]) && !char.IsPunctuation(normalized[index]))
                return null;
        cursor = start + word.Length;
        return start;
    }

    // Line normalization collapses interior whitespace and omits leading whitespace.
    internal static int NormalizedOffset(ReadOnlySpan<char> prefix)
    {
        var offset = 0;
        var space = false;
        foreach (var c in prefix)
        {
            if (char.IsWhiteSpace(c))
            {
                space = offset > 0;
                continue;
            }
            offset += space ? 2 : 1;
            space = false;
        }
        return offset + (space ? 1 : 0);
    }
}
