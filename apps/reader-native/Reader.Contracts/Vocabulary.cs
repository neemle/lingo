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
    ReviewState Reverse)
{
    public LemmaSnapshot? Analysis { get; init; }
    public ExpressionSnapshot? Expression { get; init; }
}

public sealed record LemmaSnapshot(string Lemma, string Surface, string PartOfSpeech, string Features,
    string Source, string SourceVersion, string? Context = null, string? DocumentId = null);

public sealed record ExpressionSnapshot(string Canonical, string Surface, string Kind, int Observations,
    string Source, string SourceVersion, string? Context = null, string? DocumentId = null);
