namespace Reader.Contracts;

public sealed record Vocabulary(
    string Id,
    string Word,
    string Translation,
    string Language,
    string TargetLanguage,
    string Context,
    string DocumentId,
    ReviewState Forward,
    ReviewState Reverse);
