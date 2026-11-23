using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private readonly OcrPageReviewFlow readerOcrReview = new();
    private bool reviewingReaderOcr;
    private OcrPageReviewFlow.Source? lastReaderOcrSource;

    private (int Page, bool Detached)? CurrentOcrReviewPage()
    {
        var session = AppState.Session;
        if (session.Document is not { } document) return null;
        var window = CurrentNativePdfWindow(document.Id);
        var page = window?.Center ?? session.OriginalPageIndex();
        if (window?.Pages.FirstOrDefault(item => item.Page == page) is { State: "detached" } detached)
            return detached.HasOcr ? (page, true) : null;
        return document.OcrLines?.Any(line => line.Page == page) == true ? (page, false) : null;
    }

    private void UpdateOcrPageReviewMenu()
    {
        var session = AppState.Session;
        var currentPage = CurrentOcrReviewPage();
        ReviewReaderOcrMenu.IsEnabled = !reviewingReaderOcr && !session.Busy && currentPage is not null;
        var current = lastReaderOcrSource;
        var reopeningPdf = reviewingReaderOcr && session.Document is { PdfPageStarts.Length: > 0 } document &&
            CurrentNativePdfWindow(document.Id) is null;
        var pageChanged = currentPage is { } page && current is not null &&
            !reopeningPdf && (current.Page != page.Page || current.Detached != page.Detached);
        if (current is not null && (current.DocumentId != session.Document?.Id || pageChanged))
        {
            readerOcrReview.CancelInference();
            OcrReviewFeedback.IsVisible = false;
            lastReaderOcrSource = current = null;
        }
        ReviewReaderOcrMenu.Text = readerOcrReview.ButtonText(current) + "…";
    }

    private async void ReviewReaderOcrClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (reviewingReaderOcr || readerOcrReview.IsRunning || session.Busy || CurrentOcrReviewPage() is not { } page ||
            session.Document is not { } document) return;
        bool IsCurrent() => readingPageActive && session.Document?.Id == document.Id &&
            (CurrentNativePdfWindow(document.Id) is not { } window || window.Center == page.Page);

        reviewingReaderOcr = true;
        UpdateOcrPageReviewMenu();
        try
        {
            var detached = page.Detached ? await session.DetachedPdfOcrAsync(document.Id, page.Page) : null;
            var revision = page.Detached ? detached?.Revision : document.CurrentRevisionId;
            if (revision is null || !IsCurrent()) return;
            var source = new OcrPageReviewFlow.Source(document.Id, page.Page, page.Detached, revision);
            lastReaderOcrSource = source;
            var applied = await readerOcrReview.RunAsync(session, Navigation, source, IsCurrent,
                ShowOcrReviewMessage, ShowOcrInferenceState);
            if (applied is not null && IsCurrent())
            {
                var updatedRevision = applied.DetachedPage?.Revision ?? session.Document?.CurrentRevisionId;
                if (updatedRevision is not null) lastReaderOcrSource = source with { Revision = updatedRevision };
            }
        }
        catch (Exception error) { if (IsCurrent()) ShowOcrReviewMessage(error.Message); }
        finally
        {
            reviewingReaderOcr = false;
            UpdateOcrPageReviewMenu();
        }
    }

    private void ShowOcrReviewMessage(string message)
    {
        OcrReviewMessage.Text = message;
        OcrReviewFeedback.IsVisible = message.Length > 0;
    }

    private void CancelOcrReviewClicked(object? sender, EventArgs args)
    {
        CancelOcrReview.IsEnabled = false;
        readerOcrReview.CancelInference();
    }

    private void ShowOcrInferenceState(bool running)
    {
        CancelOcrReview.IsVisible = CancelOcrReview.IsEnabled = running;
        OcrReviewActivity.IsVisible = OcrReviewActivity.IsRunning = running;
    }
}
