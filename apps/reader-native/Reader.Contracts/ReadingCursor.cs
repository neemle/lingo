namespace Reader.Contracts;

public sealed record ReadingCursor(string RevisionId, string SentenceId, int Position,
    string? TokenId, int? SentenceStartUtf16, int? TokenLengthUtf16);
