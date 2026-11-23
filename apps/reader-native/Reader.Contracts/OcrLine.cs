using System.Text.Json.Serialization;

namespace Reader.Contracts;

public sealed record OcrSuggestion(int StartUtf16, int LengthUtf16, string Source, string Replacement);
public sealed record OcrLineEdit(int Index, string Source, string Replacement);
public sealed record OcrPageReview(int CheckedLines, OcrLineEdit[] Corrections, int[] ReviewedIndices, int RemainingLines,
    int PendingInBatch = 0);

public sealed record OcrLine(
    string Text, double X, double Y, double Width, double Height, double? Confidence, int Page = 0,
    string? Correction = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] OcrWord[]? Words = null,
    OcrSpan? SourceSpan = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)] bool BlockStart = false,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? Rotation = null);

public sealed record OcrWord(string Text, double X, double Y, double Width, double Height,
    int? RawStartUtf16 = null, OcrSpan? SourceSpan = null);

public sealed record OcrSpan(int StartUtf8, int EndUtf8, int StartUtf16, int EndUtf16);
public sealed record PdfSampleBox(double X, double Y, double Width, double Height);
public sealed record PdfSampleColors(uint Background, uint? Ink);
public sealed record PdfTranslationSourceRow(int StartUtf16, int EndUtf16, int? BlockIndex, bool Pending);
public sealed record PdfTranslationSourceStyle(string? FontFamily, double? FontHeight, int Weight, bool Italic,
    double[]? Ink, double[] Background);
public sealed record PdfTranslationStyleAnchor(int StartUtf16, PdfTranslationSourceStyle Style);
public sealed record PdfTranslationAnchor(PdfSampleBox? Bounds, PdfTranslationSourceStyle? Style,
    PdfTranslationMask[]? PendingMasks = null);
public sealed record PdfTranslationMask(PdfSampleBox Bounds, double[] Background);
public sealed record PdfTranslationRegion(PdfSampleBox Bounds, double PreferredFontHeight, bool LimitFontByWidth,
    int[] Fragments, int? StyleFragment, PdfTranslationMask[] Masks, string Alignment = "left", int Rotation = 0,
    PdfTranslationRegion[]? FallbackRegions = null);
public sealed record PdfTranslationLayout(PdfTranslationRegion[] Regions, int[] Pending, int[] Overflow,
    PdfTranslationAnchor[] Anchors);
public sealed record PdfTextLine(int Start, int End);
public sealed record PdfTextFit(float Size, float LineHeight, PdfTextLine[] Lines);
