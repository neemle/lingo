namespace Reader.Contracts;

public sealed record ReadingSession(
    string Id,
    string DocumentId,
    long StartedAt,
    long EndedAt,
    long ActiveDurationSeconds);
