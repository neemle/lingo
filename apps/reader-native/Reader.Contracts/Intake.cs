namespace Reader.Contracts;

public sealed record IntakeArticleDraft(string Title, string Text, string SourceUrl);

public sealed record IntakeItem(string Id, string Name, string Kind, long Bytes,
    string NextAction, string? DocumentId, long ReceivedAt);

public sealed record IntakeFile(string Kind, string Path);
public sealed record IntakeBytes(string Kind, int[] Bytes);
public sealed record IntakeLink(string Kind, string Url);

public sealed record PdfOcrRequest(int Page, string Token, long ExpiresAt, string? Language = null, bool ReadTextLayer = false);

public sealed record PdfReadPage(int Page, string State, string Text, OcrSpan? Span, bool RefreshPending = false,
    DocumentBlock[]? Blocks = null, bool HasOcr = false);

public sealed record PdfReadWindow(int Center, int PageCount, PdfReadPage[] Pages, string Context, OcrSpan? ContextSpan);

public sealed record DetachedPdfSentence(Sentence Sentence, int Position, int Total, string Language,
    bool Bookmarked = false, string? Note = null)
{
    public Dictionary<string, string> ReviewedTranslations { get; init; } = [];
    public string? RevisionId { get; init; }
    public SpeechResume? SpeechResume { get; init; }
    public string? SpeechText { get; init; }
    public string? NarrationText { get; init; }
    public string? PronunciationText { get; init; }
    public bool Footnote { get; init; }
}

public sealed record DetachedPdfOcrPage(string Revision, OcrLine[] Lines);

public sealed record DetachedPdfAnnotationItem(int Position, string Text, bool Bookmarked, string? Note);

public sealed record DetachedPdfFindCursor(int Page, int Position);
public sealed record DetachedPdfFindHit(int Page, int Position, string SentenceId, string Text,
    string? PivotLanguage = null, bool? Reviewed = null, string? SourceText = null);
public sealed record DetachedPdfTranslation(string Text, string? PivotLanguage, string? SourceText, int StartPosition);
public sealed record DetachedPdfPassage(DetachedPdfTranslation? Active, DetachedPdfPassageRow[] Rows);
public sealed record DetachedPdfPassageRow(int Position, int SentenceCount, string Text);
public sealed record PdfPageTranslationRow(int StartUtf16, int EndUtf16, string? Text,
    string? SourceText = null, string? PivotLanguage = null, string? SourceSentenceId = null);
public sealed record PdfTextOrderEdit(int StartUtf16, int EndUtf16, string Replacement);
public sealed record DetachedPdfFindPage(DetachedPdfFindHit[] Matches, DetachedPdfFindCursor? Next);
