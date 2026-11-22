namespace Reader.Contracts;

public sealed record PracticeProgress(ulong Revision, string Direction, byte Mode, SavedQuestion? Question,
    PracticeCollection? Collection = null);

public sealed record PracticeCollection(string DocumentId, string? SourceHash = null);

public sealed record SavedQuestion(string CardId, string Prompt, string[] Answers, string[] Options,
    bool Revealed = false, string? GivenAnswer = null, long? ElapsedMs = null);
