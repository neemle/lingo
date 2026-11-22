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
    public string? SourceUrl { get; init; }
    public string? CurrentRevisionId { get; init; }
    public DocumentBlock[] Blocks { get; init; } = [];
    public OcrLine[]? OcrLines { get; init; }
    public int? OcrPdfPages { get; init; }
    public int?[]? PdfPageStarts { get; init; }
    public SpeechResume? SpeechResume { get; init; }
    public string[] Bookmarks { get; init; } = [];
    public Dictionary<string, string> Notes { get; init; } = [];
    public Dictionary<string, Dictionary<string, string>> ReviewedTranslations { get; init; } = [];
}

public sealed record SpeechResume(string SentenceId, string TextHash, int OffsetUtf16);

public sealed record DocumentRevision(string Id, string DocumentId, string? ParentId, string SourceHash,
    string ParserVersion, string TextHash, int CanonicalBytes, long CreatedAt);
