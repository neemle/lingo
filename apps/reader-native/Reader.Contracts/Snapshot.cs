namespace Reader.Contracts;

public sealed record Snapshot(
    Document[] Documents,
    Vocabulary[] Vocabulary);
