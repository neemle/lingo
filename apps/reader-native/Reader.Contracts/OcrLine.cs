namespace Reader.Contracts;

public sealed record OcrLine(
    string Text, double X, double Y, double Width, double Height, double? Confidence, int Page = 0);
