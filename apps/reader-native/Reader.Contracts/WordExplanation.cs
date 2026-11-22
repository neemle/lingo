namespace Reader.Contracts;

public sealed record WordExplanation(string Explanation);

public sealed record SentenceReview(string? Spelling, string? Syntax);
