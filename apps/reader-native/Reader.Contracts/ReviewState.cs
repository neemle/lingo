namespace Reader.Contracts;

public sealed record ReviewState(
    long DueAt = 0,
    uint IntervalDays = 0,
    uint Reviews = 0,
    FsrsMemory? Fsrs = null);

public sealed record FsrsMemory(
    float Stability,
    float Difficulty,
    long LastReviewedAt,
    uint HistoryReviews);
