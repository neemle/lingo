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
    string Format);
