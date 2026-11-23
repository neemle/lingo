namespace Reader.Contracts;

public sealed record StorageUsage(long DatabaseBytes, long OriginalBytes, long DownloadBytes,
    long DatabaseCacheBytes, long AudioCacheBytes);

public sealed record BackgroundTranslationProgress(long? ResumeAt, int? TranslatedPage = null,
    TranslationPair[]? MissingPackages = null, string[]? TranslatedSentenceIds = null,
    int? PendingPdfTextPage = null, bool WaitingForPdfOcr = false);

public sealed record BackgroundTranslatedPage(string DocumentId, string RevisionId, int Page, string Target);
public sealed record BackgroundTranslatedText(string DocumentId, string RevisionId, string Target, string[] SentenceIds);

public sealed record TranslationWarmStep(int? Next, bool Incomplete);
