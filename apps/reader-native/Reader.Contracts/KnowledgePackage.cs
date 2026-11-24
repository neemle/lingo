namespace Reader.Contracts;

public sealed record KnowledgePackage(string Language, string Name, string Version, string SourceUrl,
    string DownloadUrl, string Revision, string License, long Bytes, string Sha256, bool Installed,
    bool ContextAvailable, bool ExpressionsAvailable)
{
    public bool SyntaxAvailable { get; init; }
    public bool DependencyParseAvailable { get; init; }
}

public sealed record WordAnalysis(string Lemma, string PartOfSpeech, string Features, int Observations,
    int ContextObservations)
{
    public string? GrammarEvidence { get; init; }
}

public sealed record WordContext(string Sentence, int StartUtf16);

public sealed record ExpressionMatch(string Text, string Canonical, string Kind, int Observations,
    string? SourceName = null, string? SourceRevision = null)
{
    public int[] PartStartsUtf16 { get; init; } = [];
    public string[] TokenIds { get; init; } = [];
}

public sealed record SyntaxEvidence(string Relation, bool SelectedIsHead, string OtherWord,
    int OtherStartUtf16, int Observations);

public sealed record SpellingSuggestion(string Replacement, int Observations, int PreviousObservations,
    int NextObservations, bool HighConfidence);

public sealed record WordLookup(KnowledgePackage Package, int TotalObservations, WordAnalysis[] Analyses,
    ExpressionMatch[] Expressions)
{
    public LexicalPackage? LexicalPackage { get; init; }
    public WordMeaning[] Meanings { get; init; } = [];
    public string? SyntaxHint { get; init; }
    public SyntaxEvidence[] SyntaxEvidence { get; init; } = [];
    public SpellingSuggestion[] SpellingSuggestions { get; init; } = [];
}
