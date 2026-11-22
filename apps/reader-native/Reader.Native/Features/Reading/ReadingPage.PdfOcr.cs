#if MACCATALYST
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;
#endif

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private async void PreviousPdfPageClicked(object? sender, EventArgs args) => await MovePdfPageAsync(-1);
    private async void NextPdfPageClicked(object? sender, EventArgs args) => await MovePdfPageAsync(1);

    private Task MovePdfPageAsync(int direction)
    {
#if MACCATALYST
        return OpenAdjacentPdfPageAsync(direction);
#else
        return Task.CompletedTask;
#endif
    }

    private void SyncReaderPdf()
    {
#if MACCATALYST
        var session = AppState.Session;
        UpdatePdfPageButtons();
        if (!readingPageActive || session.Busy || ReferenceEquals(pdfSnapshot, session.Document)) return;

        var document = session.Document;
        if (document is not { Format: "pdf", PdfPageStarts.Length: > 0 })
        {
            StopReaderPdf();
            return;
        }

        var entering = pdfSnapshot?.Id != document.Id;
        if (entering)
        {
            StopReaderPdf();
            PdfPageControls.IsVisible = true;
            PdfPageStatus.Text = "Відкриваємо PDF…";
            ShowPdfPageNotice("Завантажуємо текст сторінки…");
        }
        pdfSnapshot = document;
        pdfWindowLoading = true;
        UpdatePdfPageButtons();
        pdfWindowRead = ReadPdfWindowAsync(session, document, entering);
#endif
    }

    private void StopReaderPdf()
    {
#if MACCATALYST
        pdfSnapshot = null;
        pdfWindow = null;
        pdfWindowLoading = false;
        pdfOcrFocus = null;
        pdfOcrError = null;
        CancelReaderPdfOcr();
        PdfPageControls.IsVisible = false;
        ShowPdfPageNotice(null);
#endif
    }

#if MACCATALYST
    private Document? pdfSnapshot;
    private PdfReadWindow? pdfWindow;
    private Task pdfWindowRead = Task.CompletedTask;
    private (string Id, int Page, string Language)? pdfOcrFocus;
    private CancellationTokenSource? readerPdfOcr;
    private string? pdfOcrError;
    private DateTimeOffset pdfOcrRetryAt;
    private bool movingPdfPage;
    private bool pdfWindowLoading;

    private async Task ReadPdfWindowAsync(ReaderSession session, Document document, bool entering)
    {
        var fallback = session.OriginalPageIndex();
        try
        {
            var savedPage = await session.PdfReadingPageAsync(document.Id);
            var page = savedPage ?? fallback;
            if (!IsCurrentPdfSnapshot(session, document)) return;
            if (entering)
            {
                // Restored reading positions may have no OCR requests yet.
                if (savedPage is not null && page != fallback)
                    await session.OpenPdfReadingPageAsync(document.Id, revisionId: null, page: page);
                else
                    await session.SetPdfReadingPageAsync(document.Id, page);
                if (!IsCurrentPdfSnapshot(session, document)) return;
            }
            var window = await session.PdfReadWindowAsync(document.Id, page);
            if (!IsCurrentPdfSnapshot(session, document)) return;
            var pageChanged = pdfWindow?.Center != window.Center;
            pdfWindow = window;
            var focus = (document.Id, window.Center, document.Language);
            if (pdfOcrFocus != focus)
            {
                CancelReaderPdfOcr();
                pdfOcrFocus = focus;
                pdfOcrError = null;
                pdfOcrRetryAt = default;
                StartReaderPdfOcr(session, document);
            }
            var revealingText = PdfPageNotice.IsVisible;
            UpdatePdfPageDisplay();
            if (!PdfPageNotice.IsVisible && (pageChanged || revealingText))
            {
                var span = window.Pages.FirstOrDefault(item => item.Page == window.Center)?.Span;
                if (span is not null)
                {
                    var cursor = SourceText.HighlightStart;
                    var offset = cursor >= span.StartUtf16 && cursor < span.EndUtf16 ? cursor : span.StartUtf16;
                    Dispatcher.Dispatch(() =>
                    {
                        if (IsCurrentPdfSnapshot(session, document) && !PdfPageNotice.IsVisible)
                            SourceText.RevealOffset(offset);
                    });
                }
                UpdateTranslation();
            }
        }
        catch (Exception error)
        {
            if (!IsCurrentPdfSnapshot(session, document)) return;
            PdfPageStatus.Text = "Не вдалося завантажити сторінку PDF";
            ShowPdfPageNotice(error.Message);
        }
        finally
        {
            if (IsCurrentPdfSnapshot(session, document))
            {
                pdfWindowLoading = false;
                UpdatePdfPageButtons();
            }
        }
    }

    private bool IsCurrentPdfSnapshot(ReaderSession session, Document document) =>
        readingPageActive && ReferenceEquals(pdfSnapshot, document) && ReferenceEquals(session.Document, document);

    private async Task OpenAdjacentPdfPageAsync(int direction)
    {
        var session = AppState.Session;
        if (movingPdfPage || pdfWindowLoading || session.Busy || pdfWindow is not { } window ||
            session.Document is not { } document || document.Id != pdfSnapshot?.Id) return;
        var next = window.Center + direction;
        if (next < 0 || next >= window.PageCount) return;

        movingPdfPage = true;
        CancelReaderPdfOcr();
        StopSpeech();
        CancelScheduledTranslation();
        selection = null;
        SetSelectedSense(null);
        WordCard.IsVisible = false;
        UpdatePdfPageButtons();
        try
        {
            await session.OpenPdfReadingPageAsync(document.Id, revisionId: null, page: next);
            if (session.Error.Length == 0) await pdfWindowRead;
        }
        finally
        {
            movingPdfPage = false;
            if (readingPageActive && session.Document?.Id == document.Id)
            {
                if (session.Error.Length > 0) StartReaderPdfOcr(session, document);
                UpdatePdfPageButtons();
            }
        }
    }

    private void UpdatePdfPageDisplay()
    {
        if (pdfWindow is not { } window) return;
        var page = Array.Find(window.Pages, candidate => candidate.Page == window.Center);
        var message = page?.State switch
        {
            "textLayer" or "recognized" => null,
            "empty" => "На цій сторінці не знайдено тексту.",
            "needsOcr" when pdfOcrError is not null => pdfOcrError,
            "needsOcr" => readerPdfOcr is not null ? "Розпізнаємо текст цієї сторінки…" : "Текст сторінки ще не розпізнано.",
            _ => "Для цієї сторінки немає тексту. Відкрийте оригінал.",
        };
        PdfPageStatus.Text = $"Сторінка {window.Center + 1} із {window.PageCount}";
        if (pdfOcrError is not null && message is null)
            PdfPageStatus.Text += " · помилка OCR сусідньої сторінки";
        ShowPdfPageNotice(message);
        UpdatePdfPageButtons();
    }

    private void ShowPdfPageNotice(string? message)
    {
        var show = message is not null;
        PdfPageMessage.Text = message;
        if (show && !PdfPageNotice.IsVisible)
        {
            StopSpeech();
            CancelScheduledTranslation();
            selection = null;
            SetSelectedSense(null);
            WordCard.IsVisible = false;
        }
        PdfPageNotice.IsVisible = show;
        ReadingColumns.IsVisible = ReaderControls.IsVisible = !show;
        SourceHint.IsVisible = !show && SourcePane.IsVisible;
    }

    private void UpdatePdfPageButtons()
    {
        var enabled = !movingPdfPage && !pdfWindowLoading && !AppState.Session.Busy;
        PreviousPdfPage.IsEnabled = enabled && pdfWindow is { Center: > 0 };
        NextPdfPage.IsEnabled = enabled && pdfWindow is { } window && window.Center + 1 < window.PageCount;
        RetryPdfOcr.IsVisible = enabled && readerPdfOcr is null
            && pdfWindow?.Pages.Any(page => page.State == "needsOcr") == true;
    }

    private void StartReaderPdfOcr(ReaderSession session, Document document)
    {
        if (readerPdfOcr is not null || document.OcrPdfPages is null || pdfWindow is not { } window ||
            !window.Pages.Any(page => page.State == "needsOcr")) return;
        var run = new CancellationTokenSource();
        readerPdfOcr = run;
        _ = RecognizeReaderPdfAsync(session, document, window.Center, run);
    }

    private async Task RecognizeReaderPdfAsync(
        ReaderSession session, Document document, int page, CancellationTokenSource run)
    {
        var cancellation = run.Token;
        try
        {
            var delay = pdfOcrRetryAt - DateTimeOffset.UtcNow;
            if (delay > TimeSpan.Zero) await Task.Delay(delay, cancellation);
            if (await session.OriginalFileAsync(document.Id) is not { } original)
                throw new InvalidDataException("Оригінал PDF для розпізнавання недоступний.");
            cancellation.ThrowIfCancellationRequested();
            await PdfPageOcr.RecognizeWindowAsync(session, document, original.Path, page, cancellation, async () =>
            {
                await session.WaitUntilIdleAsync();
                if (!cancellation.IsCancellationRequested && readingPageActive && session.Document?.Id == document.Id)
                    await session.RefreshAsync();
            }, updateFocus: false);
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) {}
        catch (Exception error) when (!cancellation.IsCancellationRequested)
        {
            pdfOcrError = error.Message;
            pdfOcrRetryAt = DateTimeOffset.UtcNow.AddSeconds(30);
        }
        catch (Exception error) when (cancellation.IsCancellationRequested)
        {
            System.Diagnostics.Debug.WriteLine(error.Message);
        }
        finally
        {
            if (readerPdfOcr == run)
            {
                readerPdfOcr = null;
                UpdatePdfPageDisplay();
            }
            run.Dispose();
        }
    }

    private void CancelReaderPdfOcr()
    {
        var run = readerPdfOcr;
        readerPdfOcr = null;
        run?.Cancel();
    }
#endif

    private void RetryPdfOcrClicked(object? sender, EventArgs args)
    {
#if MACCATALYST
        var session = AppState.Session;
        if (session.Document is not { } document || document.Id != pdfSnapshot?.Id) return;
        pdfOcrError = null;
        StartReaderPdfOcr(session, document);
        UpdatePdfPageDisplay();
#endif
    }
}
