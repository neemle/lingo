namespace Reader.Contracts;

public sealed record Snapshot(
    Document[] Documents,
    Vocabulary[] Vocabulary)
{
    public Dictionary<string, int> PdfReadingPositions { get; init; } = [];
}
