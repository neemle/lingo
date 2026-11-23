using Reader.Contracts;
using Reader.Presentation;

namespace Reader.Native.Services;

internal static class PdfPageOcr
{
    private static readonly SemaphoreSlim Worker = new(1, 1);

    internal static async Task ReplaceTextLayerAsync(ReaderSession session, Document document, string path,
        int page, string revision, int pageCount, CancellationToken cancellation)
    {
        await Worker.WaitAsync(cancellation);
        try
        {
            OcrLine[] lines = [];
            await PdfScan.RecognizeAsync(path, document.Language, pageCount, [page],
                (_, recognized) => lines = recognized, cancellation);
            cancellation.ThrowIfCancellationRequested();
            await session.ReplacePdfTextLayerAsync(document.Id, page, revision, document.Language, lines);
        }
        finally { Worker.Release(); }
    }

    internal static async Task RecognizeWindowAsync(ReaderSession session, Document document,
        string path, int page, CancellationToken cancellation, Func<Task> onCached,
        bool updateFocus = true, bool retryNow = false)
    {
        if (document.OcrPdfPages is not int count) return;
        await Worker.WaitAsync(cancellation);
        try
        {
            cancellation.ThrowIfCancellationRequested();
            if (updateFocus) await session.SetPdfReadingPageAsync(document.Id, page);
            Exception? firstFailure = null;
            for (var attempt = 0; attempt < 3; attempt++)
            {
                cancellation.ThrowIfCancellationRequested();
                if (await session.ClaimPdfOcrPageAsync(document.Id, page, retryNow && attempt == 0) is not { } claim) break;
                try
                {
                    var textLayer = claim.ReadTextLayer
                        ? await PdfScan.ReadPageTextAsync(path, claim.Page, count, cancellation) : null;
                    OcrLine[] lines = [];
                    if (string.IsNullOrWhiteSpace(textLayer))
                    {
                        textLayer = null;
                        await PdfScan.RecognizeAsync(path, claim.Language ?? document.Language, count, [claim.Page],
                            (_, recognized) => lines = recognized, cancellation);
                    }
                    cancellation.ThrowIfCancellationRequested();
                    await session.CachePdfOcrPageAsync(document.Id, claim, lines, textLayer);
                }
                catch (Exception error)
                {
                    await session.ReleasePdfOcrPageAsync(document.Id, claim, cancellation.IsCancellationRequested);
                    cancellation.ThrowIfCancellationRequested();
                    firstFailure ??= new InvalidDataException($"Сторінка {claim.Page + 1} · {error.Message}", error);
                    continue;
                }
                if (!cancellation.IsCancellationRequested)
                    await onCached();
            }
            cancellation.ThrowIfCancellationRequested();
            if (firstFailure is not null) throw firstFailure;
        }
        finally { Worker.Release(); }
    }
}
