namespace Reader.Native.Services;

internal sealed record PdfBoxAppearance(string? FontFamily, double RelativeFontSize, int Weight, bool Italic,
    Color? Ink, Color Background, bool MaskSafe = true);
