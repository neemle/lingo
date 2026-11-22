using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private (string Document, ReadingCursor Cursor)? canonicalWord;
    private (string Document, string Revision, string Sentence, int Offset)? loadingReadingWord;

    private DictionarySourceLocation? dictionaryFocus;

    internal async Task RevealDictionarySourceAsync(DictionarySourceLocation source)
    {
#if MACCATALYST
        await pdfWindowRead;
        if (source.Page is int page)
        {
            await detachedInitialSentence;
            if (!readingPageActive || detachedAddress is not { } address ||
                address.Document != source.DocumentId || address.Page != page) return;
            if (detachedSentence?.Sentence.Id != source.SentenceId)
                await LoadDetachedSentenceAsync(address, source.Position, source.SentenceId);
            if (detachedSentence is not { } current || current.RevisionId != source.RevisionId ||
                current.Sentence.Id != source.SentenceId) return;
            dictionaryFocus = source;
            var focus = DictionaryFocus(address.Document, current.RevisionId, current.Sentence, page);
            DetachedPdfText.HighlightStart = focus?.StartUtf16 ?? current.Sentence.StartUtf16;
            DetachedPdfText.HighlightLength = focus?.LengthUtf16 ?? current.Sentence.EndUtf16 - current.Sentence.StartUtf16;
            DetachedPdfText.RevealOffset(DetachedPdfText.HighlightStart);
            UpdateInlinePdfHighlight();
            return;
        }
#endif
        var session = AppState.Session;
        if (!readingPageActive || session.Document is not { } document || document.Id != source.DocumentId ||
            document.CurrentRevisionId != source.RevisionId || session.Sentence?.Id != source.SentenceId) return;
        dictionaryFocus = source;
        UpdateHighlight();
        SourceText.RevealOffset(SourceText.HighlightStart);
    }

    private TextSelection? DictionaryFocus(string document, string? revision, Sentence sentence, int? page)
    {
        if (dictionaryFocus is not { Selection: { } selected } source || source.DocumentId != document ||
            source.RevisionId != revision || source.SentenceId != sentence.Id || source.Page != page) return null;

        var relative = selected.StartUtf16 - sentence.StartUtf16;
        if (relative < 0 || relative > sentence.Text.Length - selected.Text.Length ||
            !sentence.Text.AsSpan(relative, selected.Text.Length).SequenceEqual(selected.Text)) return null;
        return new TextSelection(selected.StartUtf16, selected.Text.Length, selected.Text);
    }

    private async Task RestoreReadingWordAsync()
    {
        var session = AppState.Session;
        if (!readingPageActive || session.Busy || session.Sentence is not { } sentence
            || session.Document is not { CurrentRevisionId: { } revision, ReadingTokenUtf16: int offset } document
            || ReadingWord(document, sentence) is not null)
            return;

        var key = (document.Id, revision, sentence.Id, offset);
        if (loadingReadingWord == key) return;
        loadingReadingWord = key;
        try
        {
            var cursor = await session.ReadingCursorAsync(document.Id);
            if (!readingPageActive || session.Document is not { } current || current.Id != document.Id
                || current.CurrentRevisionId != revision || session.Sentence?.Id != sentence.Id
                || current.ReadingTokenUtf16 != offset || cursor?.SentenceStartUtf16 != offset)
                return;
            if (cursor is not null)
            {
                canonicalWord = (document.Id, cursor);
                UpdateHighlight();
            }
        }
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error.Message);
        }
        finally
        {
            if (loadingReadingWord == key) loadingReadingWord = null;
        }
    }

    private (int Start, int Length)? ReadingWord(Document document, Sentence sentence)
    {
        if (canonicalWord is not { } saved || saved.Document != document.Id
            || saved.Cursor.RevisionId != document.CurrentRevisionId || saved.Cursor.SentenceId != sentence.Id
            || saved.Cursor.SentenceStartUtf16 is not int offset || document.ReadingTokenUtf16 != offset
            || saved.Cursor.TokenLengthUtf16 is not int length || offset < 0 || length <= 0
            || offset > sentence.EndUtf16 - sentence.StartUtf16 - length)
            return null;
        var start = sentence.StartUtf16 + offset;
        if (offset > sentence.Text.Length - length) return null;
        var selection = new TextSelection(start, length, sentence.Text.Substring(offset, length));
        return SourceRanges.TrimFootnoteReference(selection, document.Blocks) is { } lexical
            ? (lexical.StartUtf16, lexical.LengthUtf16) : null;
    }
}
