namespace Reader.Contracts;

public sealed record MachineTranslation(string Text, string? PivotLanguage, bool JoinedLines = false, string? SourceText = null,
    bool PersonalDictionary = false);

public sealed record TranslationFindHit(int Position, string Text);
public sealed record TranslationFindPage(TranslationFindHit[] Matches, int? NextStart);
