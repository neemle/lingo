namespace Reader.Contracts;

public sealed record DictionaryGloss(string Text, string Language);
public sealed record WordExplanation(string Explanation, bool Cached = false, DictionaryGloss? Gloss = null,
    string? ModelGloss = null);

public sealed record SentenceReview(string? Spelling, string? Syntax, bool Cached = false, bool LocalRule = false);
public sealed record OcrReview(string? Replacement, bool Cached = false);
public sealed record ModelPassageTranslation(string Text, bool Cached = false, string? SourceText = null,
    string ModelName = "Qwen 2B");
public sealed record OfflinePassageTranslation(string Text, string SourceText, string? PivotLanguage);
