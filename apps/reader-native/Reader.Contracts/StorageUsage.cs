namespace Reader.Contracts;

public sealed record StorageUsage(long DatabaseBytes, long OriginalBytes, long DownloadBytes,
    long DatabaseCacheBytes, long AudioCacheBytes);

public sealed record TranslationWarmStep(int? Next, bool Incomplete);
