using System.Text.Json.Serialization;

namespace Reader.Contracts;

public sealed record OcrSuggestion(int StartUtf16, int LengthUtf16, string Source, string Replacement);
public sealed record OcrLineEdit(int Index, string Source, string Replacement);
public sealed record OcrPageReview(int CheckedLines, OcrLineEdit[] Corrections, int[] ReviewedIndices, int RemainingLines);

public sealed record OcrLine(
    string Text, double X, double Y, double Width, double Height, double? Confidence, int Page = 0,
    string? Correction = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] OcrWord[]? Words = null,
    OcrSpan? SourceSpan = null);

public sealed record OcrWord(string Text, double X, double Y, double Width, double Height,
    int? RawStartUtf16 = null, OcrSpan? SourceSpan = null);

public sealed record OcrSpan(int StartUtf8, int EndUtf8, int StartUtf16, int EndUtf16);
