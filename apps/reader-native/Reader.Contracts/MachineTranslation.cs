namespace Reader.Contracts;

public sealed record MachineTranslation(string Text, string? PivotLanguage, bool JoinedLines = false);
