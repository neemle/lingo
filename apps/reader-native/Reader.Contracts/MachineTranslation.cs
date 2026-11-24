namespace Reader.Contracts;

public sealed record MachineTranslation(string Text, string? PivotLanguage, bool JoinedLines = false, string? SourceText = null,
    bool PersonalDictionary = false, string? DirectModelRevision = null, TranslationAlignment? Alignment = null);

public sealed record TranslationAlignment(string Kind, string[] SourceSentenceIds, OcrSpan[] TargetSentences);

public sealed record TranslationFindFragment(int Page, int StartUtf16, int EndUtf16, string? PivotLanguage = null,
    bool ReflowOnly = false);
public sealed record TranslationFindHit(int Position, string Text, TranslationFindFragment? PdfFragment = null,
    TranslationAlignment? Alignment = null, string? PivotLanguage = null, string? SourceText = null);
public sealed record TranslationFindPage(TranslationFindHit[] Matches, int? NextStart,
    string? SourceRevisionId = null, string? SourceLanguage = null);
