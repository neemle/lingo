namespace Reader.Native.Services;

internal static class PdfRecognition
{
    // The Rust bridge represents PDF page counts and indices as unsigned 16-bit integers.
    internal const int MaxPages = ushort.MaxValue;

    internal static void CheckPageCount(int count)
    {
        if (count is < 1 or > MaxPages)
            throw new InvalidDataException($"Для розпізнавання PDF має містити від 1 до {MaxPages} сторінок.");
    }
}
