using Reader.Contracts;
#if MACCATALYST
using Reader.Native.Services;
using Reader.Presentation;
#endif

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private PdfReadWindow? CurrentNativePdfWindow(string documentId)
    {
#if MACCATALYST
        return pdfSnapshot?.Id == documentId ? pdfWindow : null;
#else
        return null;
#endif
    }

    private async void PreviousPdfPageClicked(object? sender, EventArgs args) => await MovePdfPageAsync(-1);
    private async void NextPdfPageClicked(object? sender, EventArgs args) => await MovePdfPageAsync(1);

    private Task MovePdfPageAsync(int direction)
    {
#if MACCATALYST
        return pdfWindow is { } window ? OpenPdfPageAsync(window.Center + direction) : Task.CompletedTask;
#else
        return Task.CompletedTask;
#endif
    }

    private async void JumpPdfPageClicked(object? sender, EventArgs args)
    {
#if MACCATALYST
        if (pdfPagePromptOpen || !JumpPdfPage.IsEnabled || pdfWindow is not { } window
            || pdfSnapshot is not { } document) return;

        pdfPagePromptOpen = true;
        UpdatePdfPageButtons();
        try
        {
            var destination = await PdfPagePrompt.ChooseAsync(this, window.Center, window.PageCount);
            if (destination is int page && readingPageActive && document.Id == pdfSnapshot?.Id)
                await OpenPdfPageAsync(page);
        }
        finally
        {
            pdfPagePromptOpen = false;
            UpdatePdfPageButtons();
        }
#else
        await Task.CompletedTask;
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
        CancelDetachedSpeechTravel();
#if MACCATALYST
        pdfSnapshot = null;
        pendingUnreadPdfFind = null;
        attemptedPdfTextOrderRepair = null;
        pdfWindow = null;
        pdfWindowLoading = false;
        pdfOcrFocus = null;
        pdfOcrError = null;
        CancelReaderPdfOcr();
        repairingPdfText?.Cancel();
        PdfPageControls.IsVisible = false;
        UpdatePdfPageButtons();
        ShowPdfPageNotice(null);
#endif
    }

#if MACCATALYST
    private Document? pdfSnapshot;
    private PdfReadWindow? pdfWindow;
    private Task pdfWindowRead = Task.CompletedTask;
    private (string Id, int Page, string Language)? pdfOcrFocus;
    private CancellationTokenSource? readerPdfOcr;
    private Task readerPdfOcrTask = Task.CompletedTask;
    private Task detachedInitialSentence = Task.CompletedTask;
    private string? pdfOcrError;
    private DateTimeOffset pdfOcrRetryAt;
    private bool movingPdfPage;
    private bool pdfWindowLoading;
    private bool pdfPagePromptOpen;
    private CancellationTokenSource? repairingPdfText;
    private (string Document, int Page, string Text)? attemptedPdfTextOrderRepair;

    private async void BackgroundPdfTextCached(string id, int page)
    {
        var session = AppState.Session;
        if (pdfSnapshot is not { } document || document.Id != id || !IsCurrentPdfSnapshot(session, document))
            return;

        await session.WaitUntilIdleAsync();
        await pdfWindowRead;
        if (!IsCurrentPdfSnapshot(session, document) ||
            pdfWindow is not { } window || Math.Abs(window.Center - page) > 1)
            return;

        pdfWindowLoading = true;
        UpdatePdfPageButtons();
        pdfWindowRead = ReadPdfWindowAsync(session, document, entering: false);
        await pdfWindowRead;
        if (!IsCurrentPdfSnapshot(session, document) || pdfWindow?.Center != window.Center)
            return;

        StartReaderPdfOcr(session, document);
        pdfAutoAttempts = null;
        if (pdfTranslationLayout) RefreshCachedPdfPageLayout();
        else if (DetachedPdfReader.IsVisible) await RestoreDetachedTranslationAsync();
    }

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
            if (await RepairStoredPdfTextOrderAsync(session, document, window))
            {
                if (IsCurrentPdfSnapshot(session, document)) await session.RefreshAsync();
                return;
            }
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
            ScheduleUnreadPdfFind();
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

    private async Task<bool> RepairStoredPdfTextOrderAsync(ReaderSession session, Document document, PdfReadWindow window)
    {
        var page = window.Pages.FirstOrDefault(candidate => candidate.Page == window.Center);
        if (document.CurrentRevisionId is not { } revision ||
            page is not { HasOcr: false, State: "textLayer" or "detached" } ||
            !PdfOrderedText.MayNeedRepair(page.Text))
            return false;

        var attempt = (document.Id, page.Page, page.Text);
        if (attemptedPdfTextOrderRepair == attempt) return false;
        attemptedPdfTextOrderRepair = attempt;
        try
        {
            if (await session.OriginalFileAsync(document.Id) is not { } original ||
                !IsCurrentPdfSnapshot(session, document)) return false;
            var corrections = await PdfScan.ReadStoredTextOrderFixesAsync(original.Path, page.Page, page.Text);
            return corrections.Length > 0 && IsCurrentPdfSnapshot(session, document) &&
                await session.RepairStoredPdfTextOrderAsync(document.Id, page.Page, revision, page.Text, corrections);
        }
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error.Message);
            return false;
        }
    }

    private async Task OpenPdfPageAsync(int page, CancellationTokenSource? narration = null)
    {
        if (narration is null) CancelDetachedSpeechTravel();
        else if (!ReferenceEquals(detachedSpeechTravel, narration)) return;
        var session = AppState.Session;
        if (movingPdfPage || pdfWindowLoading || session.Busy || pdfWindow is not { } window ||
            session.Document is not { } document || document.Id != pdfSnapshot?.Id) return;
        if (page == window.Center || page < 0 || page >= window.PageCount) return;

        pendingUnreadPdfFind = null;
        movingPdfPage = true;
        StopDetachedPdfReader();
        StopAnalysisPrefetch();
        CancelReaderPdfOcr();
        StopSpeech();
        CancelScheduledTranslation();
        selection = null;
        SetSelectedSense(null);
        WordCard.IsVisible = false;
        UpdatePdfPageButtons();
        try
        {
            await session.OpenPdfReadingPageAsync(document.Id, revisionId: null, page: page);
            if (session.Error.Length == 0) await pdfWindowRead;
        }
        finally
        {
            movingPdfPage = false;
            if (readingPageActive && session.Document?.Id == document.Id)
            {
                if (session.Error.Length > 0)
                {
                    StartReaderPdfOcr(session, document);
                    UpdatePdfPageDisplay();
                }
                UpdatePdfPageButtons();
            }
        }
    }

    private async Task OpenDetachedOcrWordCardAsync(string documentId, int page, TextSelection selected)
    {
        await pdfWindowRead;
        if (!readingPageActive || detachedAddress is not { } address || address.Document != documentId ||
            address.Page != page) return;
        await detachedInitialSentence;
        if (!ReferenceEquals(detachedAddress, address) || detachedSentence is not { } current ||
            selected.StartUtf16 < current.Sentence.StartUtf16 || selected.LengthUtf16 <= 0 ||
            selected.StartUtf16 > current.Sentence.EndUtf16 - selected.LengthUtf16 ||
            selected.StartUtf16 > address.Text.Length - selected.LengthUtf16 ||
            !address.Text.AsSpan(selected.StartUtf16, selected.LengthUtf16).SequenceEqual(selected.Text)) return;
        await ShowDetachedSelectionAsync(selected);
    }

    private async Task<int?> PdfNarrationBoundaryAsync(string documentId, Sentence completed, CancellationToken token)
    {
        var session = AppState.Session;
        if (session.Document is not { Format: "pdf", PdfPageStarts.Length: > 0 }) return null;

        await pdfWindowRead.WaitAsync(token);
        if (!readingPageActive || session.Document is not { } document || document.Id != documentId ||
            session.Sentence?.Id != completed.Id || pdfWindow is not { } window ||
            window.Center + 1 >= window.PageCount)
            return null;

        var page = window.Pages.FirstOrDefault(candidate => candidate.Page == window.Center);
        if (page?.Span is not { } span || completed.StartUtf16 >= span.EndUtf16 ||
            completed.EndUtf16 <= span.StartUtf16)
            return null;

        var next = document.Sentences.ElementAtOrDefault(document.Position + 1);
        return next is null || next.StartUtf16 >= span.EndUtf16 ? window.Center : null;
    }

    private async Task ContinuePdfNarrationAsync(string documentId, int completedPage, string? completedSentenceId = null)
    {
        if (pdfWindow?.Center != completedPage) return;
        using var narration = new CancellationTokenSource();
        CancelDetachedSpeechTravel();
        detachedSpeechTravel = narration;
        var token = narration.Token;
        var session = AppState.Session;
        try
        {
            while (readingPageActive && ContinueSpeech.IsChecked &&
                session.Document?.Id == documentId &&
                pdfWindow is { } window && window.Center + 1 < window.PageCount)
            {
                var nextPage = window.Center + 1;
                await session.WaitUntilIdleAsync().WaitAsync(token);
                await pdfWindowRead.WaitAsync(token);
                token.ThrowIfCancellationRequested();
                if (pdfWindow?.Center != window.Center || session.Document?.Id != documentId ||
                    !readingPageActive || !ContinueSpeech.IsChecked) return;
                await OpenPdfPageAsync(nextPage, narration);
                token.ThrowIfCancellationRequested();
                if (pdfWindow is not { } opened || opened.Center != nextPage) return;

                var page = opened.Pages.FirstOrDefault(candidate => candidate.Page == nextPage);
                if (page is { State: "needsOcr" or "needsTextLayer" } or { State: "empty", RefreshPending: true })
                {
                    if (readerPdfOcr is null) return;
                    await readerPdfOcrTask.WaitAsync(token);
                    await pdfWindowRead.WaitAsync(token);
                    page = pdfWindow?.Pages.FirstOrDefault(candidate => candidate.Page == nextPage);
                }
                if (!readingPageActive || session.Document?.Id != documentId || !ContinueSpeech.IsChecked)
                    return;
                if (page is { State: "empty", RefreshPending: false }) continue;

                if (page is { State: "detached" } &&
                    detachedAddress is { } target && target.Document == documentId && target.Page == nextPage)
                {
                    await detachedInitialSentence.WaitAsync(token);
                    if (detachedSentence is { Position: > 0 } && detachedAddress == target)
                        await LoadDetachedSentenceAsync(target, 0, automatic: true).WaitAsync(token);
                    token.ThrowIfCancellationRequested();
                    if (ReferenceEquals(detachedSpeechTravel, narration) && ContinueSpeech.IsChecked &&
                        detachedAddress == target && detachedSentence is { Position: 0 })
                    {
                        detachedSpeechTravel = null;
                        _ = SpeakDetachedAsync(resumePlayback: false);
                    }
                }
                else if (page?.Span is { } span)
                {
                    if (!OverlapsPdfPage(session.Sentence, span))
                    {
                        if (!await session.OpenPdfReadingPageAsync(documentId, revisionId: null, page: nextPage))
                            return;
                        await pdfWindowRead.WaitAsync(token);
                    }
                    token.ThrowIfCancellationRequested();
                    if (!readingPageActive || !ContinueSpeech.IsChecked || session.Document is not { } document ||
                        document.Id != documentId || !OverlapsPdfPage(session.Sentence, span))
                        return;

                    if (completedSentenceId is not null && session.Sentence?.Id == completedSentenceId)
                    {
                        var nextPosition = document.Position + 1;
                        var next = document.Sentences.ElementAtOrDefault(nextPosition);
                        if (!OverlapsPdfPage(next, span)) continue;
                        await session.SelectSentenceAsync(nextPosition);
                        token.ThrowIfCancellationRequested();
                        if (session.Error.Length > 0) throw new InvalidOperationException(session.Error);
                        if (session.Document?.Id != documentId || session.Sentence?.Id != next?.Id) return;
                    }
                    detachedSpeechTravel = null;
                    await SpeakAsync();
                }
                return;
            }
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) {}
        catch (Exception error)
        {
            if (ReferenceEquals(detachedSpeechTravel, narration) && readingPageActive)
                PdfPageStatus.Text = error.Message;
        }
        finally
        {
            if (ReferenceEquals(detachedSpeechTravel, narration)) detachedSpeechTravel = null;
        }
    }

    private static bool OverlapsPdfPage(Sentence? sentence, OcrSpan span) =>
        sentence is not null && sentence.StartUtf16 < span.EndUtf16 && sentence.EndUtf16 > span.StartUtf16;

    private void UpdatePdfPageDisplay()
    {
        if (pdfWindow is not { } window) return;
        var page = Array.Find(window.Pages, candidate => candidate.Page == window.Center);
        var waiting = page is { State: "needsOcr" or "needsTextLayer" } or { State: "empty", RefreshPending: true };
        var pendingMessage = pdfOcrError ?? (readerPdfOcr is not null
            ? "Готуємо текст цієї сторінки…"
            : "Текст сторінки ще не отримано.");
        var message = waiting ? pendingMessage : page?.State switch
        {
            "textLayer" or "recognized" or "detached" => null,
            "empty" => "На цій сторінці не знайдено тексту.",
            _ => "Для цієї сторінки немає тексту. Відкрийте оригінал.",
        };
        JumpPdfPage.Text = $"Сторінка {window.Center + 1} / {window.PageCount}";
        PdfPageStatus.Text = "";
        if (page is { State: "recognized" or "detached" or "textLayer", RefreshPending: true })
            PdfPageStatus.Text += pdfOcrError is not null ? "Помилка оновлення OCR"
                : readerPdfOcr is not null ? "Оновлюємо OCR" : "OCR потребує оновлення";
        else if (pdfOcrError is not null && message is null)
            PdfPageStatus.Text += "Помилка OCR сусідньої сторінки";
        if (repairingPdfText is not null)
            PdfPageStatus.Text = page?.HasOcr == true ? "Звіряємо текст" : "Розпізнаємо зображення";
        ShowPdfPageNotice(message, page is { State: "detached" } ? page.Text : null);
        if (page is { State: "detached" } && pdfSnapshot is { } document)
            ShowDetachedPdfPage(document, page);
        UpdatePdfPageButtons();
        if (showInlineOriginal) _ = ShowInlineOriginalAsync();
    }

    private void ShowPdfPageNotice(string? message, string? detachedText = null)
    {
        var show = message is not null || detachedText is not null;
        var stateChanged = PdfPageNotice.IsVisible != show;
        PdfPageMessage.Text = message;
        PdfPageMessage.IsVisible = message is not null;
        if (detachedText is not null)
            DetachedPdfText.ReadingFontSize = SourceText.ReadingFontSize;
        DetachedPdfText.Text = detachedText ?? "";
        DetachedPdfReader.IsVisible = detachedText is not null;
        if (detachedText is null) StopDetachedPdfReader();
        if (show && stateChanged)
        {
            StopSpeech();
            CancelScheduledTranslation();
            selection = null;
            SetSelectedSense(null);
            WordCard.IsVisible = false;
        }
        PdfPageNotice.IsVisible = show;
        ReadingColumns.IsVisible = ReaderControls.IsVisible = SentenceToolbar.IsVisible = !show;
        SourceHint.IsVisible = !show && SourcePane.IsVisible;
        UpdateSentenceMenu();
        if (stateChanged) SyncAnalysisPrefetch();
    }

    private void UpdatePdfPageButtons()
    {
        var enabled = !movingPdfPage && !pdfWindowLoading && !pdfPagePromptOpen &&
            repairingPdfText is null && !AppState.Session.Busy;
        JumpPdfPageMenu.IsEnabled = JumpPdfPage.IsEnabled = enabled && pdfWindow is { PageCount: > 1 };
        PreviousPdfPage.IsEnabled = enabled && pdfWindow is { Center: > 0 };
        NextPdfPage.IsEnabled = enabled && pdfWindow is { } window && window.Center + 1 < window.PageCount;
        RetryPdfOcr.IsVisible = enabled && readerPdfOcr is null
            && pdfWindow?.Pages.Any(RequiresPdfOcr) == true;
        var pageState = pdfWindow is { } current
            ? current.Pages.FirstOrDefault(page => page.Page == current.Center)
            : null;
        RepairPdfText.IsVisible = pageState is { State: "textLayer" } or { HasOcr: true };
        RepairPdfText.Text = pageState?.HasOcr == true ? "Звірити OCR" : "Розпізнати зі скану";
        RepairPdfText.IsEnabled = enabled && RepairPdfText.IsVisible;
    }

    private static bool RequiresPdfOcr(PdfReadPage page) => page.State is "needsOcr" or "needsTextLayer" || page.RefreshPending;

    private void StartReaderPdfOcr(ReaderSession session, Document document)
    {
        if (readerPdfOcr is not null || document.OcrPdfPages is null || pdfWindow is not { } window ||
            !window.Pages.Any(RequiresPdfOcr)) return;
        var run = new CancellationTokenSource();
        readerPdfOcr = run;
        readerPdfOcrTask = RecognizeReaderPdfAsync(session, document, window.Center, run);
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
#else
    private static Task<int?> PdfNarrationBoundaryAsync(string documentId, Sentence completed, CancellationToken token) =>
        Task.FromResult<int?>(null);

    private static Task ContinuePdfNarrationAsync(string documentId, int completedPage, string? completedSentenceId = null) =>
        Task.CompletedTask;
#endif

    private async void RepairPdfTextClicked(object? sender, EventArgs args)
    {
#if MACCATALYST
        var session = AppState.Session;
        if (!RepairPdfText.IsEnabled || pdfWindow is not { } window ||
            session.Document is not { CurrentRevisionId: { } revision } document ||
            window.Pages.FirstOrDefault(page => page.Page == window.Center) is not { } page) return;

        using var run = new CancellationTokenSource();
        repairingPdfText = run;
        UpdatePdfPageDisplay();
        try
        {
            if (await session.OriginalFileAsync(document.Id) is not { } original)
                throw new InvalidDataException("Оригінал PDF для розпізнавання недоступний.");
            if (page.HasOcr)
            {
                var isDetached = page.State == "detached";
                if (isDetached)
                {
                    var ocr = await session.DetachedPdfOcrAsync(document.Id, window.Center)
                        ?? throw new InvalidDataException("Розпізнаний текст сторінки недоступний.");
                    revision = ocr.Revision;
                }
                var reference = await PdfScan.ReadPageTextAsync(original.Path, window.Center, window.PageCount, run.Token);
                run.Token.ThrowIfCancellationRequested();
                var changed = await session.ReconcilePdfOcrPageAsync(
                    document.Id, window.Center, revision, document.Language, reference, isDetached);
                if (!changed && readingPageActive && !run.IsCancellationRequested && session.Document?.Id == document.Id)
                {
                    await DisplayAlertAsync("Звірка OCR", "Не знайдено розбіжностей, які можна впевнено виправити.", "Гаразд");
                    return;
                }
            }
            else
            {
                await PdfPageOcr.ReplaceTextLayerAsync(session, document, original.Path, window.Center,
                    revision, window.PageCount, run.Token);
            }
            await session.WaitUntilIdleAsync();
            if (!run.IsCancellationRequested && session.Document?.Id == document.Id) await session.RefreshAsync();
        }
        catch (OperationCanceledException) when (run.IsCancellationRequested) { }
        catch (Exception error) when (readingPageActive && !run.IsCancellationRequested &&
            session.Document?.Id == document.Id && pdfWindow?.Center == window.Center)
        {
            await DisplayAlertAsync("Розпізнавання сторінки", error.Message, "Гаразд");
        }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
        finally
        {
            repairingPdfText = null;
            UpdatePdfPageDisplay();
        }
#endif
    }

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
