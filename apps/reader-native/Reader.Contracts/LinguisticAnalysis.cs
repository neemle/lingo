namespace Reader.Contracts;

public sealed record SentenceTokenPage(string RevisionId, string SentenceId, SentenceToken[] Tokens, int? NextStart);

public sealed record SentenceToken(string Id, OcrSpan Span, int SentenceStartUtf16);

public sealed record LinguisticAnalysis(string RevisionId, string SentenceId, string Language,
    KnowledgePackage? Package, AnalyzedWord[] Words, ExpressionMatch[] Expressions,
    LinguisticSyntaxLink[] SyntaxCandidates, DependencyParse? SyntaxParse, int? NextStart)
{
    public SpacingSuggestion[] SpacingSuggestions { get; init; } = [];
    public string[] SyntaxHints { get; init; } = [];
}

public sealed record SpacingSuggestion(OcrSpan Span, string Source, string Replacement);

public sealed record AnalyzedWord(string TokenId, string Text, OcrSpan Span, WordAnalysis[] Morphology,
    string SpellingStatus, SpellingSuggestion[] SpellingSuggestions);

public sealed record LinguisticSyntaxLink(string DependentTokenId, string HeadTokenId,
    string DependentText, string HeadText, string Relation, int Observations, string? Basis);

public sealed record DependencyParse(string Status, string? RootText,
    LinguisticSyntaxLink[] Links, string[] UnassignedTokenIds);
