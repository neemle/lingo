using Reader.Contracts;
using Reader.Presentation;

namespace Reader.Native.Services;

internal static class PdfPageOcr
{
    private static readonly SemaphoreSlim Worker = new(1, 1);

    internal static async Task RecognizeWindowAsync(ReaderSession session, Document document,
        string path, int page, CancellationToken cancellation, Func<Task> onCached, bool updateFocus = true)
    {
        if (document.OcrPdfPages is not int count) return;
        await Worker.WaitAsync(cancellation);
        try
        {
            cancellation.ThrowIfCancellationRequested();
            if (updateFocus) await session.SetPdfReadingPageAsync(document.Id, page);
            for (var attempt = 0; attempt < 3; attempt++)
            {
                cancellation.ThrowIfCancellationRequested();
                if (await session.ClaimPdfOcrPageAsync(document.Id) is not { } claim) return;
                try
                {
                    OcrLine[] lines = [];
                    await PdfScan.RecognizeAsync(path, document.Language, count, [claim.Page],
                        (_, recognized) => lines = recognized, cancellation);
                    cancellation.ThrowIfCancellationRequested();
                    await session.CachePdfOcrPageAsync(document.Id, claim, lines);
                }
                catch
                {
                    await session.ReleasePdfOcrPageAsync(document.Id, claim, cancellation.IsCancellationRequested);
                    throw;
                }
                if (!cancellation.IsCancellationRequested)
                    await onCached();
            }
        }
        finally { Worker.Release(); }
    }
}
