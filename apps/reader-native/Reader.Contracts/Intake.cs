namespace Reader.Contracts;

public sealed record IntakeItem(string Id, string Name, string Kind, long Bytes,
    string NextAction, string? DocumentId);

public sealed record IntakeFile(string Kind, string Path);

public sealed record PdfOcrRequest(int Page, string Token, long ExpiresAt);
