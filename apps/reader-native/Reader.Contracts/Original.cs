namespace Reader.Contracts;

public sealed record Original(
    string DocumentId,
    string Name,
    int[] Bytes);
