namespace Reader.Contracts;

public sealed record DictionaryGloss(string Text, string Language);
public sealed record WordExplanation(string Explanation, bool Cached = false, DictionaryGloss? Gloss = null);

public sealed record SentenceReview(string? Spelling, string? Syntax, bool Cached = false);
public sealed record ModelPassageTranslation(string Text, bool Cached = false, string? SourceText = null);
