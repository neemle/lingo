namespace Reader.Contracts;

public sealed record PracticeProgress(ulong Revision, string Direction, byte Mode, SavedQuestion? Question,
    PracticeCollection? Collection = null);

public sealed record PracticeCollection(string DocumentId, string? SourceHash = null);

public sealed record PracticeSettings(string Direction, byte Mode, PracticeCollection? Collection);

public sealed record SavedQuestion(string CardId, string Prompt, string[] Answers, string[] Options,
    bool Revealed = false, string? GivenAnswer = null, long? ElapsedMs = null);

public sealed record PracticeQueue(PracticeProgress Progress, int DueCount, int TotalCards, bool? MatchesDictionary,
    string? Context = null);

public sealed record PracticeAnswer(PracticeProgress Progress, bool? MatchesDictionary);
