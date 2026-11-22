#if MACCATALYST
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private CancellationTokenSource? readerPdfOcr;
    private (string DocumentId, int Page)? readerPdfFocus;

    private void QueueReaderPdfOcr()
    {
        var session = AppState.Session;
        if (!readingPageActive || session.Document is not
            { Format: "pdf", OcrPdfPages: > 0 } document)
            return;
        var page = session.Sentence is null ? -1 : session.OriginalPageIndex();
        if (readerPdfFocus == (document.Id, page)) return;
        readerPdfFocus = (document.Id, page);
        readerPdfOcr?.Cancel();
        var run = new CancellationTokenSource();
        readerPdfOcr = run;
        _ = RecognizeReaderPdfAsync(session, document, page, run);
    }

    private async Task RecognizeReaderPdfAsync(ReaderSession session, Document document, int page,
        CancellationTokenSource run)
    {
        try
        {
            if (await session.OriginalFileAsync(document.Id) is not { } original) return;
            if (page < 0) page = await session.PdfReadingPageAsync(document.Id) ?? 0;
            await PdfPageOcr.RecognizeWindowAsync(session, document, original.Path, page, run.Token, async () =>
            {
                if (readingPageActive && session.Document?.Id == document.Id && !session.Busy)
                    await session.RefreshAsync();
            });
        }
        catch (OperationCanceledException) when (run.IsCancellationRequested) {}
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error.Message);
        }
        finally
        {
            if (readerPdfOcr == run) readerPdfOcr = null;
            run.Dispose();
        }
    }

    private void StopReaderPdfOcr()
    {
        readerPdfFocus = null;
        readerPdfOcr?.Cancel();
        readerPdfOcr = null;
    }
}
#endif
