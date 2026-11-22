namespace Reader.Contracts;

public sealed record PracticeProgress(ulong Revision, string Direction, byte Mode, SavedQuestion? Question);

public sealed record SavedQuestion(string CardId, string Prompt, string[] Answers, string[] Options,
    bool Revealed = false, string? GivenAnswer = null);
