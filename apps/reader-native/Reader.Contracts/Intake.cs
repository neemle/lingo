namespace Reader.Contracts;

public sealed record IntakeItem(string Id, string Name, string Kind, long Bytes,
    string NextAction, string? DocumentId, long ReceivedAt);

public sealed record IntakeFile(string Kind, string Path);
public sealed record IntakeBytes(string Kind, int[] Bytes);
public sealed record IntakeLink(string Kind, string Url);

public sealed record PdfOcrRequest(int Page, string Token, long ExpiresAt);

public sealed record PdfReadPage(int Page, string State, string Text, OcrSpan? Span);

public sealed record PdfReadWindow(int Center, int PageCount, PdfReadPage[] Pages, string Context, OcrSpan? ContextSpan);
