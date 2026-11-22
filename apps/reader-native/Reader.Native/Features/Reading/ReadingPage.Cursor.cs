using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private (string Document, ReadingCursor Cursor)? canonicalWord;
    private (string Document, string Revision, string Sentence, int Offset)? loadingReadingWord;

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
        return (sentence.StartUtf16 + offset, length);
    }
}
