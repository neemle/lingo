namespace Reader.Contracts;

public sealed record Document(
    string Id,
    string Title,
    string Language,
    string Text,
    Sentence[] Sentences,
    int Position,
    long CreatedAt,
    string SourceHash,
    string Format)
{
    public OcrLine[]? OcrLines { get; init; }
    public string[] Bookmarks { get; init; } = [];
    public Dictionary<string, Dictionary<string, string>> ReviewedTranslations { get; init; } = [];
}
