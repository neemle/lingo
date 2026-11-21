namespace Reader.Contracts;

public sealed record ReviewEvent(
    string EntryId,
    string Direction,
    byte Grade,
    long ReviewedAt);
