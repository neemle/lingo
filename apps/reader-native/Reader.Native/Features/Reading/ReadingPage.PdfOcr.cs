using Reader.Contracts;
#if MACCATALYST || IOS
using Reader.Native.Services;
using Reader.Presentation;
#endif

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private PdfReadWindow? CurrentNativePdfWindow(string documentId)
    {
#if MACCATALYST || IOS
        return pdfSnapshot?.Id == documentId ? pdfWindow : null;
#else
        return null;
#endif
    }

    private async void PreviousPdfPageClicked(object? sender, EventArgs args) => await MovePdfPageAsync(-1);
    private async void NextPdfPageClicked(object? sender, EventArgs args) => await MovePdfPageAsync(1);

    private Task MovePdfPageAsync(int direction)
    {
#if MACCATALYST || IOS
        return pdfWindow is { } window ? OpenPdfPageAsync(window.Center + direction) : Task.CompletedTask;
#else
        return Task.CompletedTask;
#endif
    }

    private bool HasAdjacentPdfPage(int direction)
    {
#if MACCATALYST || IOS
        return pdfWindow is { } window && window.Center + direction >= 0 &&
            window.Center + direction < window.PageCount;
#else
        return false;
#endif
    }

    private int? ActivePdfPageForSentence(string document, Sentence? sentence)
    {
#if MACCATALYST || IOS
        if (CurrentNativePdfWindow(document) is { } window &&
            Array.Find(window.Pages, page => page.Page == window.Center)?.Span is { } span &&
            OverlapsPdfPage(sentence, span))
            return window.Center;
#endif
        return null;
    }

    private bool AtPdfSentenceBoundary(int direction)
    {
#if MACCATALYST || IOS
        var session = AppState.Session;
        if (session.Document is not { Format: "pdf" } document || pdfSnapshot?.Id != document.Id ||
            pdfWindow is not { } window || !HasAdjacentPdfPage(direction))
            return false;

        if (movingPdfPage) return true;
        var page = Array.Find(window.Pages, candidate => candidate.Page == window.Center);
        return page?.Span is { } span && OverlapsPdfPage(session.Sentence, span) &&
            !OverlapsPdfPage(document.Sentences.ElementAtOrDefault(document.Position + direction), span);
#else
        return false;
#endif
    }

    private async Task MoveToAdjacentPdfSentenceAsync(int direction)
    {
#if MACCATALYST || IOS
        var session = AppState.Session;
        if (movingPdfPage || pdfWindowLoading || session.Busy || repairingPdfText is not null ||
            !HasAdjacentPdfPage(direction) ||
            session.Document is not { } source || pdfSnapshot?.Id != source.Id)
            return;

        var sourceSentenceId = detachedAddress is null ? session.Sentence?.Id : null;
        using var travel = new CancellationTokenSource();
        CancelDetachedSpeechTravel();
        detachedSpeechTravel = travel;
        try
        {
            if (await FindPdfSentencePageAsync(source, direction, travel) is not { } page) return;
            var destination = page.Page;
            if (page is { State: "detached" } && detachedAddress is { } address &&
                address.Document == source.Id && address.Page == destination)
            {
                await detachedInitialSentence.WaitAsync(travel.Token);
                travel.Token.ThrowIfCancellationRequested();
                if (ReferenceEquals(detachedAddress, address) && address.Language == source.Language)
                    await LoadDetachedSentenceAsync(address, position: null, automatic: true,
                        sourceOffset: direction > 0 ? 0 : address.Text.Length);
            }
            else if (page?.Span is { } span && session.Document is { } document && !session.Busy)
            {
                var position = PdfSentenceNavigation.ArrivalPosition(document.Sentences, span, direction, sourceSentenceId);
                if (position >= 0) await session.SelectSentenceAsync(position, pdfPage: destination);
            }
        }
        catch (OperationCanceledException) when (travel.IsCancellationRequested) {}
        catch (Exception error)
        {
            if (ReferenceEquals(detachedSpeechTravel, travel) && readingPageActive)
                PdfPageStatus.Text = error.Message;
        }
        finally
        {
            if (ReferenceEquals(detachedSpeechTravel, travel)) detachedSpeechTravel = null;
        }
#else
        await Task.CompletedTask;
#endif
    }

    private async void JumpPdfPageClicked(object? sender, EventArgs args)
    {
#if MACCATALYST || IOS
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
#if MACCATALYST || IOS
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
#if MACCATALYST || IOS
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

#if MACCATALYST || IOS
    private Document? pdfSnapshot;
    private PdfReadWindow? pdfWindow;
    private Task pdfWindowRead = Task.CompletedTask;
    private (string Id, int Page, string Language)? pdfOcrFocus;
    private CancellationTokenSource? readerPdfOcr;
    private Task readerPdfOcrTask = Task.CompletedTask;
    private TaskCompletionSource? sentencePageUpdated;
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
            var (changed, scanOverlay) = await RepairStoredPdfLayoutAsync(session, document, window);
            if (changed)
            {
                if (!IsCurrentPdfSnapshot(session, document)) return;
                if (!window.Pages.Any(item => item.Page == window.Center && item.State == "detached"))
                {
                    await session.RefreshAsync();
                    return;
                }
                window = await session.PdfReadWindowAsync(document.Id, page);
            }
            if (!IsCurrentPdfSnapshot(session, document)) return;
            var previousWindow = pdfWindow;
            var pageChanged = previousWindow?.Center != window.Center;
            pdfWindow = window;
            var translationChanged = InvalidatePdfTranslationContext(document, previousWindow, window);
            var focus = (document.Id, window.Center, document.Language);
            var focusChanged = pdfOcrFocus != focus;
            if (focusChanged)
            {
                CancelReaderPdfOcr();
                pdfOcrFocus = focus;
                pdfOcrError = null;
                pdfOcrRetryAt = default;
            }
            if (focusChanged || scanOverlay is not null)
                StartReaderPdfOcr(session, document, scanOverlay: scanOverlay);
            var revealingText = PdfPageNotice.IsVisible;
            UpdatePdfPageDisplay();
            sentencePageUpdated?.TrySetResult();
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
            }
            if (translationChanged || !PdfPageNotice.IsVisible && (pageChanged || revealingText)) UpdateTranslation();
            if (translationChanged)
            {
                if (pdfTranslationLayout) RefreshCachedPdfPageLayout();
                if (DetachedPdfReader.IsVisible) ResetDetachedTranslation();
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

    private bool InvalidatePdfTranslationContext(Document document, PdfReadWindow? previous, PdfReadWindow current)
    {
        if (previous is null) return false;
        var changed = current.Pages.Where(page => previous.Pages.Any(old => old.Page == page.Page &&
            (old.Text != page.Text || old.HasOcr != page.HasOcr))).Select(page => page.Page).ToArray();
        if (changed.Length == 0) return false;

        bool Affected(int page) => changed.Any(other => Math.Abs(other - page) <= 1);
        bool Invalid(PdfLayoutAddress address) => address.Document == document.Id && Affected(address.Page);
        recentPdfTranslations.RemoveAll(entry => Invalid(entry.Address));
        if (translatingPdfLayout is { } running && Invalid(running)) ReleasePdfLayoutTranslation()?.Cancel();
        if (pdfLayoutRequest is { } displayed && Invalid(displayed))
        {
            pdfLayoutRowsStale = true;
            pdfLayoutRequestId++;
        }

        var spans = previous.Pages.Concat(current.Pages).Where(page => Affected(page.Page))
            .Select(page => page.Span).OfType<OcrSpan>().ToArray();
        translationWindows.RemoveAll(window => window.Scope.Document == document.Id && window.Units.Any(unit =>
            unit.Sources.Any(sentence => spans.Any(span => OverlapsPdfPage(sentence, span)))));
        translationCacheVersion++;
        if (Affected(previous.Center) || Affected(current.Center)) pdfTranslationContextGeneration++;
        lastCacheRestore = null;
        if (!Affected(current.Center)) return false;
        pdfAutoAttempts = null;
        lastAutoTranslation = null;
        lastPrefetchOrigin = null;
        return true;
    }

    private bool IsCurrentPdfSnapshot(ReaderSession session, Document document) =>
        readingPageActive && ReferenceEquals(pdfSnapshot, document) && ReferenceEquals(session.Document, document);

    private async Task<(bool Changed, string? ScanOverlay)> RepairStoredPdfLayoutAsync(
        ReaderSession session, Document document, PdfReadWindow window)
    {
        var page = window.Pages.FirstOrDefault(candidate => candidate.Page == window.Center);
        if (document.CurrentRevisionId is not { } revision ||
            page is null || page.Text.Length == 0 ||
            page is not ({ HasOcr: true } or { State: "textLayer" or "detached" }))
            return default;

        var attempt = (document.Id, page.Page, page.Text);
        if (attemptedPdfTextOrderRepair == attempt) return default;
        attemptedPdfTextOrderRepair = attempt;
        try
        {
            if (page.HasOcr)
                return (await session.UpgradeStoredOcrLayoutAsync(document.Id, page.Page, revision, page.Text,
                    page.State == "detached"), null);
            if (await session.OriginalFileAsync(document.Id) is not { } original ||
                !IsCurrentPdfSnapshot(session, document)) return default;
            var inspection = await PdfScan.InspectStoredLayoutAsync(original.Path, page.Page, page.Text);
            if (!IsCurrentPdfSnapshot(session, document)) return default;
            if (inspection.IsScanOverlay) return (false, page.Text);
            return (inspection.Corrections.Length > 0 && await session.RepairStoredPdfTextOrderAsync(
                document.Id, page.Page, revision, page.Text, inspection.Corrections), null);
        }
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error.Message);
            return default;
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
        pdfTranslationDestination = (document.Id, page);
        RefreshPdfTranslationLayout();
        StopDetachedPdfReader();
        StopAnalysisPrefetch();
        CancelReaderPdfOcr();
        StopSpeech();
        CancelScheduledTranslation();
        CancelModelPassage();
        CancelPopupTranslation();
        CancelWordExplanation();
        if (!keepPdfTranslationOnNavigation) pdfLayoutTranslationCancellation?.Cancel();
        selection = null;
        SetSelectedSense(null);
        WordCard.IsVisible = false;
        UpdatePdfPageButtons();
        try
        {
            await session.OpenPdfReadingPageAsync(document.Id, revisionId: null, page: page);
            if (session.Error.Length == 0 && session.Document?.Id == document.Id)
            {
                RecordReadingActivity(document.Id);
                await pdfWindowRead;
            }
        }
        finally
        {
            movingPdfPage = false;
            pdfTranslationDestination = null;
            if (readingPageActive && session.Document?.Id == document.Id)
            {
                if (session.Error.Length > 0)
                {
                    StartReaderPdfOcr(session, document);
                    UpdatePdfPageDisplay();
                }
                UpdatePdfPageButtons();
                RefreshPdfTranslationLayout();
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

        Task windowRead;
        do
        {
            windowRead = pdfWindowRead;
            await windowRead.WaitAsync(token);
        }
        while (!ReferenceEquals(windowRead, pdfWindowRead));
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
        var session = AppState.Session;
        if (pdfWindow?.Center != completedPage || session.Document is not { } source || source.Id != documentId) return;
        using var narration = new CancellationTokenSource();
        CancelDetachedSpeechTravel();
        detachedSpeechTravel = narration;
        var token = narration.Token;
        try
        {
            while (await FindPdfSentencePageAsync(source, 1, narration, narrating: true) is { } page)
            {
                var nextPage = page.Page;
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
                        await session.SelectSentenceAsync(nextPosition, pdfPage: nextPage);
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

    private async Task<PdfReadPage?> FindPdfSentencePageAsync(Document source, int direction,
        CancellationTokenSource travel, bool narrating = false)
    {
        var session = AppState.Session;
        var token = travel.Token;
        bool Current(int page) => readingPageActive && ReferenceEquals(detachedSpeechTravel, travel) &&
            (!narrating || ContinueSpeech.IsChecked) && pdfWindow?.Center == page &&
            session.Document is { } active && active.Id == source.Id && active.Language == source.Language;

        while (pdfWindow is { } window && Current(window.Center) && HasAdjacentPdfPage(direction))
        {
            var next = window.Center + direction;
            await session.WaitUntilIdleAsync().WaitAsync(token);
            await pdfWindowRead.WaitAsync(token);
            token.ThrowIfCancellationRequested();
            if (!Current(window.Center)) return null;

            await OpenPdfPageAsync(next, travel);
            token.ThrowIfCancellationRequested();
            if (!Current(next)) return null;

            var page = await WaitForPdfSentencePageAsync(source.Id, next, token);
            token.ThrowIfCancellationRequested();
            if (!Current(next)) return null;

            if (page is { State: "empty", RefreshPending: false }) continue;
            return page;
        }
        return null;
    }

    private async Task<PdfReadPage?> WaitForPdfSentencePageAsync(string documentId, int page, CancellationToken token)
    {
        while (pdfSnapshot?.Id == documentId && pdfWindow is { } window && window.Center == page)
        {
            var current = Array.Find(window.Pages, candidate => candidate.Page == page);
            if (current is not ({ State: "needsOcr" or "needsTextLayer" } or { State: "empty", RefreshPending: true }))
                return current;
            if (readerPdfOcr is null) return null;

            // A window can keep processing neighbouring pages after this page is ready.
            var updated = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            sentencePageUpdated = updated;
            try
            {
                await Task.WhenAny(updated.Task, readerPdfOcrTask).WaitAsync(token);
                await pdfWindowRead.WaitAsync(token);
            }
            finally
            {
                if (ReferenceEquals(sentencePageUpdated, updated)) sentencePageUpdated = null;
            }
        }
        return null;
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
        SourceHint.IsVisible = !compactReader && !show && SourcePane.IsVisible;
        UpdateSentenceMenu();
        if (stateChanged) SyncAnalysisPrefetch();
    }

    private void UpdatePdfPageButtons()
    {
        var enabled = !movingPdfPage && !pdfWindowLoading && !pdfPagePromptOpen &&
            repairingPdfText is null && !AppState.Session.Busy;
        JumpPdfPageMenu.IsEnabled = JumpPdfPage.IsEnabled = enabled && pdfWindow is { PageCount: > 1 };
        PreviousPdfPageMenu.IsEnabled = PreviousPdfPage.IsEnabled = enabled && HasAdjacentPdfPage(-1);
        NextPdfPageMenu.IsEnabled = NextPdfPage.IsEnabled = enabled && HasAdjacentPdfPage(1);
        RetryPdfOcr.IsVisible = enabled && readerPdfOcr is null
            && pdfWindow?.Pages.Any(RequiresPdfOcr) == true;
        var pageState = pdfWindow is { } current
            ? current.Pages.FirstOrDefault(page => page.Page == current.Center)
            : null;
        RepairPdfText.IsVisible = pageState is { State: "textLayer" } or { HasOcr: true };
        RepairPdfText.Text = pageState?.HasOcr == true ? "Дії OCR…" : "Розпізнати зі скану";
        RepairPdfText.IsEnabled = enabled && RepairPdfText.IsVisible;
    }

    private static bool RequiresPdfOcr(PdfReadPage page) => page.State is "needsOcr" or "needsTextLayer" || page.RefreshPending;

    private void StartReaderPdfOcr(ReaderSession session, Document document, bool retryNow = false, string? scanOverlay = null)
    {
        if (readerPdfOcr is not null || pdfWindow is not { } window) return;
        if (scanOverlay is null && (document.OcrPdfPages is null || !window.Pages.Any(RequiresPdfOcr))) return;
        var run = new CancellationTokenSource();
        readerPdfOcr = run;
        readerPdfOcrTask = RecognizeReaderPdfAsync(
            session, document, window.Center, window.PageCount, run, retryNow, scanOverlay);
    }

    private async Task RecognizeReaderPdfAsync(
        ReaderSession session, Document document, int page, int pageCount, CancellationTokenSource run, bool retryNow,
        string? scanOverlay)
    {
        var cancellation = run.Token;
        try
        {
            var delay = pdfOcrRetryAt - DateTimeOffset.UtcNow;
            if (!retryNow && delay > TimeSpan.Zero) await Task.Delay(delay, cancellation);
            if (await session.OriginalFileAsync(document.Id) is not { } original)
                throw new InvalidDataException("Оригінал PDF для розпізнавання недоступний.");
            cancellation.ThrowIfCancellationRequested();
            if (scanOverlay is not null && document.CurrentRevisionId is { } revision)
            {
                var lines = await PdfPageOcr.RecognizePageAsync(document, original.Path, page, pageCount, cancellation);
                cancellation.ThrowIfCancellationRequested();
                if (lines.Length > 0 && readingPageActive && pdfWindow?.Center == page &&
                    session.Document is { } visible && visible.Id == document.Id &&
                    visible.CurrentRevisionId == revision && visible.Language == document.Language &&
                    await session.RecoverPdfScanOverlayAsync(document.Id, page, revision, document.Language, scanOverlay, lines))
                    await RefreshAsync();
            }
            await PdfPageOcr.RecognizeWindowAsync(session, document, original.Path, page, cancellation, RefreshAsync,
                updateFocus: false, retryNow: retryNow);

            async Task RefreshAsync()
            {
                await session.WaitUntilIdleAsync();
                if (!cancellation.IsCancellationRequested && readingPageActive && session.Document?.Id == document.Id)
                    await session.RefreshAsync();
            }
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
#if MACCATALYST || IOS
        var session = AppState.Session;
        if (!RepairPdfText.IsEnabled || pdfWindow is not { } window ||
            session.Document is not { CurrentRevisionId: { } revision } document ||
            window.Pages.FirstOrDefault(page => page.Page == window.Center) is not { } page) return;

        using var run = new CancellationTokenSource();
        repairingPdfText = run;
        UpdatePdfPageDisplay();
        try
        {
            const string supplement = "Повторно розпізнати зі скану";
            const string reconcile = "Звірити текст OCR";
            var action = page.HasOcr
                ? await DisplayActionSheetAsync("Розпізнавання сторінки", "Скасувати", null, supplement, reconcile)
                : null;
            run.Token.ThrowIfCancellationRequested();
            if (page.HasOcr && action != supplement && action != reconcile) return;
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
                bool changed;
                if (action == supplement)
                {
                    var lines = await PdfPageOcr.RecognizePageAsync(document, original.Path, window.Center,
                        window.PageCount, run.Token);
                    run.Token.ThrowIfCancellationRequested();
                    changed = await session.SupplementPdfOcrPageAsync(
                        document.Id, window.Center, revision, document.Language, isDetached, lines);
                }
                else
                {
                    var reference = await PdfScan.ReadPageTextAsync(original.Path, window.Center, window.PageCount, run.Token);
                    run.Token.ThrowIfCancellationRequested();
                    changed = await session.ReconcilePdfOcrPageAsync(
                        document.Id, window.Center, revision, document.Language, reference, isDetached);
                }
                if (!changed && readingPageActive && !run.IsCancellationRequested && session.Document?.Id == document.Id)
                {
                    var message = action == supplement ? "Нових рядків або підтверджених виправлень не знайдено."
                        : "Не знайдено розбіжностей, які можна впевнено виправити.";
                    await DisplayAlertAsync("Розпізнавання сторінки", message, "Гаразд");
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
#if MACCATALYST || IOS
        var session = AppState.Session;
        if (session.Document is not { } document || document.Id != pdfSnapshot?.Id) return;
        pdfOcrError = null;
        pdfOcrRetryAt = default;
        StartReaderPdfOcr(session, document, retryNow: true);
        UpdatePdfPageDisplay();
#endif
    }
}
