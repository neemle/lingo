namespace Reader.Contracts;

public sealed record Backup(
    uint Version,
    Snapshot Snapshot,
    Original[] Originals,
    ReviewEvent[] Reviews)
{
    public BackupResource[] Resources { get; init; } = [];
    public ReadingSession[] ReadingSessions { get; init; } = [];
    public DocumentRevision[] DocumentRevisions { get; init; } = [];
    public PracticeProgress? Practice { get; init; }
    public DictionarySource[] DictionarySources { get; init; } = [];
    public string[] DictionaryReviewOrigins { get; init; } = [];
    public ReaderPreferences ReaderPreferences { get; init; } = new();
}

public sealed record BackupResource(string Kind, string Id, string Revision);

public sealed record BackupPreview(int Documents, int Vocabulary, int Reviews, int ReadingSessions,
    int Resources, bool HasPractice);
