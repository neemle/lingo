namespace Reader.Contracts;

public sealed record Backup(
    uint Version,
    Snapshot Snapshot,
    Original[] Originals,
    ReviewEvent[] Reviews)
{
    public PracticeProgress? Practice { get; init; }
}
