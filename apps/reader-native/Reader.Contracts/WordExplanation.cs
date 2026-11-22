namespace Reader.Contracts;

public sealed record WordExplanation(string Explanation, bool Cached = false);

public sealed record SentenceReview(string? Spelling, string? Syntax, bool Cached = false);
