using Reader.Contracts;

namespace Reader.Native.Services;

internal sealed record PdfRecognition(int Pages, OcrLine[] Lines)
{
    internal const int MaxPages = 40;

    internal static void CheckPageCount(int count)
    {
        if (count is < 1 or > MaxPages)
            throw new InvalidDataException($"Для розпізнавання PDF має містити від 1 до {MaxPages} сторінок.");
    }
}
