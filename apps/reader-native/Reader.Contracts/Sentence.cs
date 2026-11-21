namespace Reader.Contracts;

public sealed record Sentence(
    string Id,
    string Text,
    int StartUtf8,
    int EndUtf8,
    int StartUtf16,
    int EndUtf16);
