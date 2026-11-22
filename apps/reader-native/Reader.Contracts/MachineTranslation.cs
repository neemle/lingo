namespace Reader.Contracts;

public sealed record MachineTranslation(string Text, string? PivotLanguage, bool JoinedLines = false);

public sealed record TranslationFindHit(int Position, string Text);
public sealed record TranslationFindPage(TranslationFindHit[] Matches, int? NextStart);
