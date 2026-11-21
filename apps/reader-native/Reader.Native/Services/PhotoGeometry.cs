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
}
