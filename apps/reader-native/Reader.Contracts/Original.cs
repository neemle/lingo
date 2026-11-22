namespace Reader.Contracts;

public sealed record Original(
    string DocumentId,
    string Name,
    int[] Bytes);

public sealed record OriginalFile(string Name, string Path);
