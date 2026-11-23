using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;
using System.Text;
using System.Text.RegularExpressions;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record PdfLayoutAddress(string Document, int Page, string? Revision, string Target, string? DetachedText)
    {
        public InlinePdfAddress Source => new(Document, Page, Revision, DetachedText);
    }
    private sealed record PdfAutoTranslation(PdfLayoutAddress Address, int RouteRequest);
    private sealed record PdfSourceLayout(InlinePdfAddress Page, OcrLine[] Lines, int Offset, Sentence? Sentence)
    {
        public bool Matches(PdfLayoutAddress? address) => address is not null &&
            Page.Document == address.Document && Page.Page == address.Page &&
            Page.Revision == address.Revision && Page.DetachedText == address.DetachedText;
    }
    private bool pdfTranslationLayout = true;
    private bool syncingPdfTranslationScroll;
    private PdfLayoutAddress? pdfLayoutRequest;
    private PdfLayoutAddress? translatingPdfLayout;
    private CancellationTokenSource? pdfLayoutTranslationCancellation;
    private PdfPageTranslationRow[] pdfLayoutRows = [];
    private PdfSourceLayout? pdfSourceLayout;
    private Sentence? pdfLayoutSentence => pdfSourceLayout is { } source && source.Matches(pdfLayoutRequest)
        ? source.Sentence : null;
    private string? pdfLayoutStatus;
    private int pdfLayoutRequestId;
    private bool refreshingPdfLayout;
    private bool openingPdfTranslationText;
    private PdfLayoutAddress? pendingPdfCacheRefresh;
    private PdfAutoTranslation? pendingPdfAutoTranslation;
    private (PdfAutoTranslation Request, int Attempts)? pdfAutoAttempts;
    private (VerticalStackLayout Panel, PdfPageTranslationRow[] Rows, Label[] Labels, Sentence? Active)? overflowContent;
    private (PdfLayoutAddress Address, string SentenceId, RectF Bounds, double ViewportHeight)? lastPdfTranslationFollow;
    private const string UntranslatedPdfFragment = "Фрагмент ще не перекладено";

    private static bool HasPdfTranslation(PdfPageTranslationRow row) => !string.IsNullOrWhiteSpace(row.Text);

    private (ScrollView Viewport, GraphicsView Surface) ActivePdfTranslationLayout() =>
        DetachedPdfReader.IsVisible
            ? (DetachedPdfTranslationViewport, DetachedPdfTranslationSurface)
            : (PdfTranslationViewport, PdfTranslationSurface);

    private bool PdfUsesPageTranslation => showInlineOriginal && pdfTranslationLayout && TranslationVisible &&
        AppState.Session.Document?.Format == "pdf";

    private void TogglePdfTranslationLayout(object? sender, EventArgs args)
    {
        pdfTranslationLayout = !pdfTranslationLayout;
        SaveReaderPreference(ReaderPreferenceChange.PdfTranslationLayout(pdfTranslationLayout));
        RefreshPdfTranslationLayout();
        ScheduleAutoTranslation();
        if (!pdfTranslationLayout && DetachedPdfReader.IsVisible) _ = RestoreDetachedTranslationAsync();
    }

    private async void TranslatePdfPageClicked(object? sender, EventArgs args)
    {
        if (PdfPageNeedsPackages)
        {
            await OpenTranslationPackagesAsync();
            return;
        }
        if (pdfLayoutRequest is { } address && translatingPdfLayout is null && PdfPageRouteReady(address))
            await LoadPdfPageLayoutAsync(address, cachedOnly: false);
    }

    private bool PdfPageRouteReady(PdfLayoutAddress address) =>
        readiness is { Ready: true } && AppState.Session.Document is { } document &&
        readinessPair == (document.Language, address.Target);

    private bool PdfPageNeedsPackages => PdfUsesPageTranslation && readiness is { Missing.Length: > 0 } &&
        CurrentPdfLayoutAddress() is { } address && AppState.Session.Document is { } document &&
        readinessPair == (document.Language, address.Target);

    private void UpdateTranslationPackageActions()
    {
        var session = AppState.Session;
        var missing = TargetCode is { } target && session.Document is { } document &&
            readinessPair == (document.Language, target) && readiness is { Missing.Length: > 0 };
        var showFooterAction = missing && !PdfPageNeedsPackages;
        OpenTranslationPackages.IsVisible = showFooterAction && session.Document is { } source &&
            session.Sentence is { } sentence && !IsFootnote(source, sentence);
        DetachedOpenTranslationPackages.IsVisible = showFooterAction && DetachedReadiness.IsVisible;
    }

    private PdfLayoutAddress? CurrentPdfLayoutAddress() =>
        showInlineOriginal && inlinePdfAddress is { } page && TargetCode is { } target &&
        AppState.Session.Document is { Format: "pdf" } document && page.Document == document.Id &&
        page.Revision == document.CurrentRevisionId &&
        target != document.Language && (!PdfPageNotice.IsVisible || DetachedPdfReader.IsVisible) &&
        (CurrentNativePdfWindow(document.Id) is not { } window || window.Center == page.Page)
            ? new(page.Document, page.Page, document.CurrentRevisionId, target, page.DetachedText)
            : null;

    private void RefreshPdfTranslationLayout()
    {
        var address = CurrentPdfLayoutAddress();
        var alreadyCached = address == pdfLayoutRequest && pdfLayoutRows.Length > 0 &&
            pdfLayoutRows.All(row => row.Text is not null);
        AppState.TranslationWorker?.SetVisiblePdf(readingPageActive && TranslationVisible &&
            !alreadyCached && !(autoTranslate && PdfUsesPageTranslation) &&
            address is { Revision: { } revision }
                ? new BackgroundTranslatedPage(address.Document, revision, address.Page, address.Target) : null,
            address?.DetachedText);
        var available = address is not null;
        var detached = DetachedPdfReader.IsVisible;
        var shown = available && pdfTranslationLayout;
        if (translatingPdfLayout is not null && (!shown || !TranslationVisible || !readingPageActive ||
            translatingPdfLayout != address))
            pdfLayoutTranslationCancellation?.Cancel();
        PdfTranslationMode.IsVisible = available && !detached;
        DetachedPdfTranslationMode.IsVisible = available && detached;
        PdfTranslationMode.Text = DetachedPdfTranslationMode.Text = shown ? "Список" : "Верстка";
        PdfTranslationViewport.IsVisible = TranslatePdfPage.IsVisible = shown && !detached;
        DetachedPdfTranslationViewport.IsVisible = DetachedTranslatePdfPage.IsVisible = shown && detached;
        TranslationViewport.IsVisible = !PdfTranslationViewport.IsVisible;
        DetachedTranslationViewport.IsVisible = !DetachedPdfTranslationViewport.IsVisible;
        var translatingPage = translatingPdfLayout == address && address is not null;
        if (address != pdfLayoutRequest)
        {
            pdfLayoutRequest = address;
            pendingPdfCacheRefresh = null;
            pdfLayoutRequestId++;
            pdfAutoAttempts = null;
            pdfLayoutRows = [];
            pdfLayoutStatus = null;
            overflowContent = null;
            lastPdfTranslationFollow = null;
            PdfTranslationOverflow.IsVisible = DetachedPdfTranslationOverflow.IsVisible = false;
            if (shown && address is not null) _ = LoadPdfPageLayoutAsync(address, cachedOnly: true);
        }
        else if (shown && address is not null && pdfLayoutRows.Length == 0 && pdfLayoutStatus is null)
            _ = LoadPdfPageLayoutAsync(address, cachedOnly: true);
        UpdatePdfTranslationStatus(shown ? address : null, detached, translatingPage);
        UpdateTranslationPackageActions();
        var hasRows = pdfLayoutRows.Length > 0;
        ReadPdfTranslationText.IsVisible = shown && !detached && hasRows;
        DetachedReadPdfTranslationText.IsVisible = shown && detached && hasRows;
        if (detached)
            DetachedShareTranslation.IsVisible = !loadingDetachedSentence && !translatingDetachedSentence &&
                DisplayedDetachedTranslationPair() is not null;
        else
            ShareTranslationPair.IsVisible = DisplayedTranslationPair() is not null;
        if (shown)
        {
            ResizePdfTranslationLayout();
            DrawPdfTranslationLayout();
        }
        SchedulePdfAutoTranslation();
    }

    private void UpdatePdfTranslationStatus(PdfLayoutAddress? address, bool detached, bool translatingPage)
    {
        var ready = address is not null && PdfPageRouteReady(address);
        var needsPackages = address is not null && PdfPageNeedsPackages;
        TranslatePdfPage.IsEnabled = DetachedTranslatePdfPage.IsEnabled =
            (ready || needsPackages) && translatingPdfLayout is null && !AppState.Session.Busy;
        TranslatePdfPage.Text = DetachedTranslatePdfPage.Text = translatingPage ? "Перекладаємо…"
            : needsPackages ? "Мовні пакети" : "Перекласти сторінку";
        string? text = null;
        if (address is not null)
        {
            if (translatingPage) text = "Перекладаємо сторінку…";
            else if (!ready && AppState.Session.Document is { } document)
                text = readinessPair == (document.Language, address.Target)
                    ? ReadinessMessage() : "Перевіряємо мовні пакети…";
            else if (pdfLayoutStatus is { Length: > 0 }) text = pdfLayoutStatus;
            var translated = pdfLayoutRows.Count(HasPdfTranslation);
            if (translated > 0 && translated < pdfLayoutRows.Length)
            {
                var progress = $"Перекладено {translated} з {pdfLayoutRows.Length} фрагментів.";
                text = text is null ? progress : $"{text} {progress}";
            }
            else if (text is null && translated == 0 && pdfLayoutRows.Length > 0)
                text = "Сторінка очікує перекладу.";
            if (PdfPagePivotNotice(address) is { } pivotNotice)
                text = text is null ? pivotNotice : $"{text} {pivotNotice}";
        }

        PdfLayoutStatusLabel.Text = DetachedPdfLayoutStatusLabel.Text = text;
        PdfLayoutStatusLabel.IsVisible = !detached && text is not null;
        DetachedPdfLayoutStatusLabel.IsVisible = detached && text is not null;
    }

    private string? PdfPagePivotNotice(PdfLayoutAddress address)
    {
        if (AppState.Session.Document is not { } document || document.Id != address.Document) return null;
        var routes = pdfLayoutRows.Where(row => row.Text is not null)
            .Select(row => row.PivotLanguage).OfType<string>().Distinct(StringComparer.Ordinal)
            .Select(pivot => DocumentLanguages.PivotRoute(document.Language, pivot, address.Target))
            .ToArray();
        return routes.Length == 0 ? null : $"Переклад через проміжну мову · {string.Join(", ", routes)}.";
    }

    private int PdfAttemptCount(PdfAutoTranslation request) =>
        pdfAutoAttempts is { } attempts && attempts.Request == request ? attempts.Attempts : 0;

    private PdfAutoTranslation? NextPdfAutoTranslation()
    {
        var session = AppState.Session;
        if (!readingPageActive || !autoTranslate || !PdfUsesPageTranslation || session.Busy ||
            inlinePdfLoading is not null || translatingPdfLayout is not null ||
            readiness is not { Ready: true } || session.Document is not { } document ||
            pdfLayoutRequest is not { } address || CurrentPdfLayoutAddress() != address ||
            readinessPair != (document.Language, address.Target) || !pdfLayoutRows.Any(row => row.Text is null))
            return null;

        var request = new PdfAutoTranslation(address, readinessRequest);
        return PdfAttemptCount(request) < 2 ? request : null;
    }

    private void SchedulePdfAutoTranslation()
    {
        var request = NextPdfAutoTranslation();
        if (pendingPdfAutoTranslation == request) return;
        pendingPdfAutoTranslation = request;
        if (request is not null) _ = TranslatePdfAfterNavigationAsync(request);
    }

    private async Task TranslatePdfAfterNavigationAsync(PdfAutoTranslation request)
    {
        await Task.Delay(PdfAttemptCount(request) == 0 ? 350 : 2500);
        if (!ReferenceEquals(pendingPdfAutoTranslation, request)) return;
        pendingPdfAutoTranslation = null;
        if (NextPdfAutoTranslation() == request)
            await LoadPdfPageLayoutAsync(request.Address, cachedOnly: false);
    }

    private async void ReadPdfTranslationTextClicked(object? sender, EventArgs args)
    {
        var rows = pdfLayoutRows;
        if (openingPdfTranslationText || rows.Length == 0 ||
            pdfLayoutRequest is not { } address || CurrentPdfLayoutAddress() != address) return;
        openingPdfTranslationText = true;
        try
        {
            var selected = await ChoosePdfTranslationSourceAsync(address, rows);
            if (selected is not null)
                await RevealPdfTranslationSourceAsync(address, selected);
        }
        catch (Exception error) { await DisplayAlertAsync("Переклад сторінки", error.Message, "Гаразд"); }
        finally { openingPdfTranslationText = false; }
    }

    private async Task<PdfPageTranslationRow?> ChoosePdfTranslationSourceAsync(
        PdfLayoutAddress address, PdfPageTranslationRow[] rows)
    {
        var result = new TaskCompletionSource<PdfPageTranslationRow?>(TaskCreationOptions.RunContinuationsAsynchronously);
        var text = new VerticalStackLayout { Padding = new Thickness(22, 10), Spacing = 9 };
        var viewport = new ScrollView { Content = text };
        if (PdfPagePivotNotice(address) is { } pivotNotice)
            text.Children.Add(new Label { Text = pivotNotice, Style = (Style)Resources["ReaderMeta"] });
        Grid? activeRow = null;
        foreach (var row in rows)
        {
            var label = PdfTranslationTextLabel(row, 20);
            var action = new Button
            {
                Text = "До оригіналу",
                Style = (Style)Resources["PlayerButton"],
                VerticalOptions = LayoutOptions.Center,
            };
            SemanticProperties.SetHint(action, "Показати відповідний фрагмент на сторінці PDF");
            action.Clicked += (_, _) => result.TrySetResult(row);
            var item = new Grid { ColumnDefinitions = { new(GridLength.Star), new(GridLength.Auto) }, ColumnSpacing = 12 };
            if (!HasPdfTranslation(row) && !string.IsNullOrWhiteSpace(row.SourceText))
            {
                var original = new Label { Text = row.SourceText, FontFamily = "Georgia", FontSize = 18 };
                item.Add(new VerticalStackLayout { Spacing = 4, Children = { original, label } });
            }
            else item.Add(label);
            item.Add(action, 1);
            if (pdfLayoutSentence is { } sentence &&
                row.StartUtf16 < sentence.EndUtf16 && row.EndUtf16 > sentence.StartUtf16)
            {
                item.BackgroundColor = Color.FromArgb("#FFF1CF");
                activeRow ??= item;
            }
            text.Children.Add(item);
        }
        var done = new Button { Text = "Закрити", HorizontalOptions = LayoutOptions.End, Margin = new Thickness(12) };
        var layout = new Grid { RowDefinitions = { new(GridLength.Auto), new(GridLength.Star) } };
        layout.Add(done);
        layout.Add(viewport, 0, 1);
        var page = new ContentPage { Title = $"Фрагменти сторінки {address.Page + 1}", Content = layout };
        page.Disappearing += (_, _) => result.TrySetResult(null);
        done.Clicked += (_, _) => result.TrySetResult(null);
        await Navigation.PushModalAsync(page);
        if (activeRow is not null) await viewport.ScrollToAsync(activeRow, ScrollToPosition.MakeVisible, animated: false);
        var selected = await result.Task;
        if (Navigation.ModalStack.LastOrDefault() == page) await Navigation.PopModalAsync();
        return selected;
    }

    private async Task RevealPdfTranslationSourceAsync(PdfLayoutAddress address, PdfPageTranslationRow fragment)
    {
        var session = AppState.Session;
        await session.WaitUntilIdleAsync();
#if MACCATALYST
        while (readingPageActive)
        {
            var window = pdfWindowRead;
            await window;
            var detached = detachedInitialSentence;
            if (address.DetachedText is not null) await detached;
            if (ReferenceEquals(window, pdfWindowRead) &&
                (address.DetachedText is null || ReferenceEquals(detached, detachedInitialSentence))) break;
        }
#endif
        if (!IsCurrentPdfTranslationSource(address)) return;
        var page = await session.PdfReadingPageAsync(address.Document) ?? session.OriginalPageIndex();
        if (page != address.Page || !IsCurrentPdfTranslationSource(address)) return;

        if (readingLayout == ReaderLayout.Translation)
        {
            readingLayout = ReaderLayout.Automatic;
            ArrangeReadingColumns();
        }
        await SelectPdfSourceRangeAsync(address.Source, fragment.StartUtf16, fragment.EndUtf16, fragment.SourceSentenceId);
    }

    private bool IsCurrentPdfTranslationSource(PdfLayoutAddress address) =>
        readingPageActive && !AppState.Session.Busy && TargetCode == address.Target &&
        AppState.Session.Document is { Format: "pdf" } document &&
        document.Id == address.Document && document.CurrentRevisionId == address.Revision;

    private Label PdfTranslationTextLabel(PdfPageTranslationRow row, double size) =>
        HasPdfTranslation(row)
            ? new Label { Text = row.Text, FontFamily = "Georgia", FontSize = size, LineHeight = 1.3 }
            : new Label { Text = UntranslatedPdfFragment, Style = (Style)Resources["ReaderMeta"] };

    private async Task LoadPdfPageLayoutAsync(PdfLayoutAddress address, bool cachedOnly)
    {
        var request = ++pdfLayoutRequestId;
        using var cancellation = cachedOnly ? null : new CancellationTokenSource();
        pdfLayoutStatus = cachedOnly ? "Завантажуємо переклад…" : "Перекладаємо сторінку…";
        if (!cachedOnly)
        {
            translatingPdfLayout = address;
            pdfLayoutTranslationCancellation = cancellation;
            var attempt = new PdfAutoTranslation(address, readinessRequest);
            pdfAutoAttempts = (attempt, PdfAttemptCount(attempt) + 1);
        }
        RefreshPdfTranslationLayout();
        try
        {
            var translation = AppState.Session.PdfPageTranslationAsync(address.Document, address.Page,
                address.Target, cachedOnly, cancellation?.Token ?? default);
            if (!cachedOnly) _ = FollowPdfTranslationBatchesAsync(address, request, translation);
            var rows = await translation;
            if (request != pdfLayoutRequestId || CurrentPdfLayoutAddress() != address) return;
            pdfLayoutRows = rows;
            pdfLayoutStatus = rows.Length == 0 ? "На сторінці немає тексту для перекладу."
                : !cachedOnly && rows.All(row => row.Text is null) ? "Переклад сторінки поки недоступний." : "";
        }
        catch (OperationCanceledException) when (cancellation?.IsCancellationRequested == true)
        {
            if (request == pdfLayoutRequestId) pdfLayoutStatus = null;
        }
        catch (Exception error)
        {
            if (request == pdfLayoutRequestId) pdfLayoutStatus = error.Message;
        }
        finally
        {
            if (!cachedOnly && translatingPdfLayout == address)
            {
                translatingPdfLayout = null;
                pdfLayoutTranslationCancellation = null;
            }
            RefreshPdfTranslationLayout();
            if (!cachedOnly && CurrentPdfLayoutAddress() == address &&
                (pendingPdfCacheRefresh == address || request != pdfLayoutRequestId))
                RefreshCachedPdfPageLayout();
        }
    }

    private async Task FollowPdfTranslationBatchesAsync(PdfLayoutAddress address, int request, Task translation)
    {
        bool Current() => !translation.IsCompleted && readingPageActive && pdfTranslationLayout &&
            request == pdfLayoutRequestId && CurrentPdfLayoutAddress() == address &&
            (pdfLayoutRows.Length == 0 || pdfLayoutRows.Any(row => row.Text is null));
        while (Current())
        {
            await Task.WhenAny(translation, Task.Delay(1200));
            if (!Current()) return;
            try
            {
                var rows = await AppState.Session.PdfPageTranslationAsync(address.Document, address.Page,
                    address.Target, cachedOnly: true);
                if (!Current()) return;
                if (rows.Any(row => row.Text is not null) && !pdfLayoutRows.SequenceEqual(rows))
                {
                    pdfLayoutRows = rows;
                    RefreshPdfTranslationLayout();
                }
            }
            catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
        }
    }

    private void PdfTranslationViewportChanged(object? sender, EventArgs args)
    {
        if (!showInlineOriginal) return;
        ResizePdfTranslationLayout();
        if (ActiveInlinePdf().Viewport.IsVisible) _ = FollowPdfTranslationScrollAsync(fromOriginal: true);
    }

    private void BackgroundPageTranslated(BackgroundTranslatedPage translated)
    {
        if (!readingPageActive || CurrentPdfLayoutAddress() is not { } address ||
            translated.DocumentId != address.Document || translated.RevisionId != address.Revision ||
            translated.Target != address.Target || translated.Page != address.Page)
            return;

        if (pdfTranslationLayout) RefreshCachedPdfPageLayout();
        else if (DetachedPdfReader.IsVisible) _ = RestoreDetachedTranslationAsync(allowAutoTranslate: false);
        else
        {
            lastCacheRestore = null;
            _ = RestoreCachedTranslationAsync();
        }
    }

    private async void RefreshCachedPdfPageLayout()
    {
        if (!readingPageActive || !pdfTranslationLayout ||
            CurrentPdfLayoutAddress() is not { } address) return;
        pendingPdfCacheRefresh = address;
        if (refreshingPdfLayout || translatingPdfLayout == address) return;
        refreshingPdfLayout = true;
        try
        {
            while (pendingPdfCacheRefresh is { } pending && pending != translatingPdfLayout)
            {
                pendingPdfCacheRefresh = null;
                await Task.Delay(150);
                if (!readingPageActive || !pdfTranslationLayout || CurrentPdfLayoutAddress() != pending) continue;
                if (translatingPdfLayout == pending)
                {
                    pendingPdfCacheRefresh = pending;
                    break;
                }
                // Updates before the read starts are already covered by this read.
                if (pendingPdfCacheRefresh == pending) pendingPdfCacheRefresh = null;
                await LoadPdfPageLayoutAsync(pending, cachedOnly: true);
            }
        }
        finally { refreshingPdfLayout = false; }
    }

    private void ResizePdfTranslationLayout()
    {
        if (inlinePdfPreview is not { Width: > 0, Height: > 0 } preview) return;
        var (viewport, _) = ActivePdfTranslationLayout();
        if (!viewport.IsVisible || viewport.Width <= 0 || viewport.Height <= 0) return;
        var scale = viewport.Width / preview.Width;
        if (!inlinePdfFitWidth) scale = Math.Min(scale, viewport.Height / preview.Height);
        var paper = DetachedPdfReader.IsVisible ? DetachedPdfTranslationPaper : PdfTranslationPaper;
        paper.WidthRequest = Math.Floor(preview.Width * scale);
        paper.HeightRequest = Math.Floor(preview.Height * scale);
    }

    private async void PdfTranslationViewportScrolled(object? sender, ScrolledEventArgs args)
    {
        if (ReferenceEquals(sender, ActivePdfTranslationLayout().Viewport))
            await FollowPdfTranslationScrollAsync(fromOriginal: false);
    }

    private async void TranslatedPdfTapped(object? sender, TappedEventArgs args)
    {
        var (viewport, surface) = ActivePdfTranslationLayout();
        if (!ReferenceEquals(sender, surface) || !viewport.IsVisible ||
            args.GetPosition(surface) is not { } point || surface.Drawable is not TranslatedPdfDrawing drawing ||
            drawing.HitTest(point) is not { } fragment) return;
        await SelectTranslatedPdfRowAsync(fragment);
    }

    private async Task SelectTranslatedPdfRowAsync(PdfPageTranslationRow fragment)
    {
        var session = AppState.Session;
        if (!readingPageActive || session.Busy || CurrentPdfLayoutAddress() is not { } address ||
            address != pdfLayoutRequest || !pdfLayoutRows.Contains(fragment)) return;
        await SelectPdfSourceRangeAsync(address.Source, fragment.StartUtf16, fragment.EndUtf16, fragment.SourceSentenceId);
    }

    private async Task SelectPdfSourceRangeAsync(InlinePdfAddress address, int start, int end, string? sentenceId = null)
    {
        var session = AppState.Session;
        if (!readingPageActive || session.Busy) return;
        lastInlinePdfFollow = null;
        if (address.DetachedText is not null)
        {
            if (detachedAddress is { } detached && detached.Document == address.Document &&
                detached.Page == address.Page && detached.Text == address.DetachedText && !loadingDetachedSentence)
                await LoadDetachedSentenceAsync(detached, position: null, sentenceId: sentenceId, sourceOffset: start);
            return;
        }
        if (DetachedPdfReader.IsVisible || session.Document is not { } document ||
            document.Id != address.Document || document.CurrentRevisionId != address.Revision) return;
        var position = Array.FindIndex(document.Sentences,
            sentence => (sentenceId is null || sentence.Id == sentenceId) &&
                sentence.StartUtf16 < end && sentence.EndUtf16 > start);
        if (position < 0) return;
        dictionaryFocus = null;
        documentFindFocus = null;
        selection = null;
        WordCard.IsVisible = false;
        if (position != document.Position)
        {
            StopSpeech();
            await session.SelectSentenceAsync(position, pdfPage: address.Page);
        }
        if (session.Error.Length == 0 && session.Sentence?.Id == document.Sentences[position].Id)
        {
            UpdateHighlight();
            SourceText.RevealOffset(start);
        }
    }

    private async Task FollowPdfTranslationScrollAsync(bool fromOriginal)
    {
        var (original, originalSurface) = ActiveInlinePdf();
        var (translated, translatedSurface) = ActivePdfTranslationLayout();
        if (!inlinePdfFitWidth || syncingPdfTranslationScroll || !original.IsVisible || !translated.IsVisible ||
            originalSurface.Height <= 0 || translatedSurface.Height <= 0) return;

        var source = fromOriginal ? original : translated;
        var destination = fromOriginal ? translated : original;
        var sourceHeight = fromOriginal ? originalSurface.Height : translatedSurface.Height;
        var destinationHeight = fromOriginal ? translatedSurface.Height : originalSurface.Height;
        var sourcePosition = source.ScrollY * destinationHeight / sourceHeight;
        if (!fromOriginal && translatedSurface.Drawable is TranslatedPdfDrawing drawing &&
            OverflowRowAtScrollPosition(translated, translatedSurface.Height) is { } row &&
            drawing.SourceTop(row) is { } top)
            sourcePosition = top * destinationHeight - Math.Min(44, destination.Height * 0.12);
        var target = Math.Clamp(sourcePosition, 0,
            Math.Max(0, destinationHeight - destination.Height));
        if (Math.Abs(destination.ScrollY - target) < 1) return;

        syncingPdfTranslationScroll = true;
        try { await destination.ScrollToAsync(0, target, animated: false); }
        catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
        finally { syncingPdfTranslationScroll = false; }
    }

    private PdfPageTranslationRow? OverflowRowAtScrollPosition(ScrollView viewport, double paperHeight)
    {
        if (overflowContent is not { } overflow || overflow.Labels.Length == 0 || !overflow.Panel.IsVisible ||
            !ReferenceEquals(overflow.Panel.Parent, viewport.Content) ||
            viewport.ScrollY <= Math.Max(0, paperHeight - viewport.Height) + 1)
            return null;

        var offset = Math.Max(0, viewport.ScrollY - overflow.Panel.Y);
        var index = Array.FindIndex(overflow.Labels, label => label.Height > 0 && label.Y + label.Height > offset);
        return index >= 0 ? overflow.Rows[index] : null;
    }

    private void DrawPdfTranslationLayout()
    {
        var (viewport, surface) = ActivePdfTranslationLayout();
        if (!viewport.IsVisible) return;
        var detached = DetachedPdfReader.IsVisible;
        var background = detached ? DetachedPdfTranslationBackground : PdfTranslationBackground;
        var original = detached ? DetachedInlinePdfImage : InlinePdfImage;
        if (!ReferenceEquals(background.Source, original.Source)) background.Source = original.Source;
        var source = pdfSourceLayout is { } layout && layout.Matches(pdfLayoutRequest) ? layout : null;
        background.IsVisible = background.Source is not null && source is { Lines.Length: > 0 } &&
            pdfLayoutRows.Length > 0;
        var gloss = (detached ? DetachedWordCard : WordCard).IsVisible ? PopupGloss(detached) : null;
        var drawing = new TranslatedPdfDrawing(pdfLayoutRows, source?.Lines ?? [], source?.Offset ?? 0,
            pdfLayoutSentence, gloss, background.IsVisible);
        var overflow = detached ? DetachedPdfTranslationOverflow : PdfTranslationOverflow;
        drawing.OverflowChanged = rows => Dispatcher.Dispatch(() =>
        {
            if (viewport.IsVisible && ReferenceEquals(surface.Drawable, drawing))
            {
                var description = string.Join(" ", pdfLayoutRows.Except(rows)
                    .Select(row => HasPdfTranslation(row) ? row.Text : UntranslatedPdfFragment));
                if (SemanticProperties.GetDescription(surface) != description)
                    SemanticProperties.SetDescription(surface, description);
                var bounds = drawing.ActiveBounds;
                ShowPdfTranslationOverflow(overflow, rows, bounds is not null);
                if (bounds is { } activeBounds && pdfLayoutSentence is { } active &&
                    !rows.Any(row => row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16))
                    _ = FollowRenderedPdfSentenceAsync(viewport, surface, active, activeBounds);
                else
                    lastPdfTranslationFollow = null;
            }
        });
        surface.Drawable = drawing;
        surface.Invalidate();
    }

    private async Task FollowRenderedPdfSentenceAsync(ScrollView viewport, GraphicsView surface, Sentence active, RectF bounds)
    {
        if (!inlinePdfFitWidth || syncingPdfTranslationScroll || viewport.Height <= 0 ||
            pdfLayoutRequest is not { } address) return;
        var key = (address, active.Id, bounds, viewport.Height);
        if (lastPdfTranslationFollow == key) return;
        lastPdfTranslationFollow = key;

        var margin = Math.Min(44, viewport.Height * 0.12);
        if (bounds.Top >= viewport.ScrollY + margin &&
            bounds.Bottom <= viewport.ScrollY + viewport.Height - margin) return;
        var target = Math.Clamp(bounds.Top - margin, 0, Math.Max(0, surface.Height - viewport.Height));
        if (Math.Abs(viewport.ScrollY - target) < 1) return;
        syncingPdfTranslationScroll = true;
        try { await viewport.ScrollToAsync(0, target, animated: false); }
        catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
        finally { syncingPdfTranslationScroll = false; }
    }

    private void ShowPdfTranslationOverflow(VerticalStackLayout panel, PdfPageTranslationRow[] rows, bool hasRenderedActive)
    {
        var active = pdfLayoutSentence;
        var previous = overflowContent;
        Label[] labels;
        if (previous is { } current && current.Panel == panel && current.Rows.SequenceEqual(rows))
        {
            if (current.Active == active) return;
            labels = current.Labels;
        }
        else
        {
            panel.Children.Clear();
            panel.IsVisible = rows.Length > 0;
            labels = new Label[rows.Length];
            if (rows.Length > 0)
            {
                var heading = new Label { Text = "Переклад під сторінкою", FontAttributes = FontAttributes.Bold };
                SemanticProperties.SetHeadingLevel(heading, SemanticHeadingLevel.Level3);
                panel.Children.Add(heading);
                for (var index = 0; index < rows.Length; index++)
                {
                    var row = rows[index];
                    var label = PdfTranslationTextLabel(row, 19);
                    var tap = new TapGestureRecognizer();
                    tap.Tapped += async (_, _) => await SelectTranslatedPdfRowAsync(row);
                    label.GestureRecognizers.Add(tap);
                    panel.Children.Add(label);
                    labels[index] = label;
                }
            }
        }
        overflowContent = (panel, rows, labels, active);

        Label? activeLabel = null;
        for (var index = 0; index < labels.Length; index++)
        {
            var selected = active is not null && rows[index].StartUtf16 < active.EndUtf16 && rows[index].EndUtf16 > active.StartUtf16;
            labels[index].BackgroundColor = selected ? Color.FromArgb("#FFF1CF") : Colors.Transparent;
            if (selected) activeLabel ??= labels[index];
        }
        var previouslyInOverflow = active is not null && previous?.Rows.Any(row =>
            row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16) == true;
        if ((activeLabel is not null || !hasRenderedActive) &&
            (previous?.Active != active || previous?.Panel != panel || previouslyInOverflow != (activeLabel is not null)))
            FollowOverflowSelection(panel, active, activeLabel);
    }

    private void FollowOverflowSelection(VerticalStackLayout panel, Sentence? active, Label? label)
    {
        if (active is null) return;
        if (label is { Height: <= 0 })
        {
            EventHandler? measured = null;
            measured = (_, _) =>
            {
                if (label.Height <= 0) return;
                label.SizeChanged -= measured;
                FollowOverflowSelection(panel, active, label);
            };
            label.SizeChanged += measured;
            return;
        }

        Dispatcher.Dispatch(async () =>
        {
            var (viewport, surface) = ActivePdfTranslationLayout();
            if (!viewport.IsVisible || syncingPdfTranslationScroll || overflowContent is not { } current ||
                current.Panel != panel || current.Active != active || (label is not null && !current.Labels.Contains(label))) return;
            if (label is null)
            {
                if (viewport.ScrollY <= Math.Max(0, surface.Height - viewport.Height) + 1) return;
                if (inlinePdfFitWidth)
                {
                    await FollowPdfTranslationScrollAsync(fromOriginal: true);
                    return;
                }
            }

            syncingPdfTranslationScroll = true;
            try
            {
                if (label is null) await viewport.ScrollToAsync(0, 0, animated: false);
                else await viewport.ScrollToAsync(label, ScrollToPosition.MakeVisible, animated: false);
            }
            catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
            finally { syncingPdfTranslationScroll = false; }
        });
    }

    private sealed class TranslatedPdfDrawing(PdfPageTranslationRow[] rows, OcrLine[] geometry, int offset,
        Sentence? active, string? gloss, bool preserveArtwork) : IDrawable
    {
        private sealed record Region(RectF Bounds, List<PdfPageTranslationRow> Rows);
        private readonly record struct Fragment(PdfPageTranslationRow Source, int Start, int End, bool Active);
        private readonly record struct WrappedLine(int Start, int End);
        private readonly List<(RectF Bounds, PdfPageTranslationRow Source)> hitTargets = [];
        internal Action<PdfPageTranslationRow[]>? OverflowChanged { get; set; }
        internal RectF? ActiveBounds { get; private set; }

        internal double? SourceTop(PdfPageTranslationRow row) => Bounds(row, new RectF(0, 0, 1, 1))?.Top;

        internal PdfPageTranslationRow? HitTest(Point point)
        {
            foreach (var (bounds, source) in hitTargets)
                if (point.X >= bounds.Left && point.X <= bounds.Right &&
                    point.Y >= bounds.Top && point.Y <= bounds.Bottom)
                    return source;
            return null;
        }

        public void Draw(ICanvas canvas, RectF page)
        {
            hitTargets.Clear();
            ActiveBounds = null;
            canvas.FillColor = Colors.White;
            if (page.Width <= 0 || page.Height <= 0) return;
            if (preserveArtwork)
            {
                foreach (var line in geometry)
                {
                    if (!HasTranslatableText(line.SourceSpan)) continue;
                    if (line.Words is { Length: > 0 } words && words.All(word => word.SourceSpan is not null) &&
                        words.Any(word => !HasTranslatableText(word.SourceSpan)))
                    {
                        foreach (var word in words)
                            if (HasTranslatableText(word.SourceSpan)) Mask(word.X, word.Y, word.Width, word.Height);
                    }
                    else Mask(line.X, line.Y, line.Width, line.Height);
                }
            }
            else canvas.FillRectangle(page);
            var regions = new List<Region>();
            var overflow = new List<PdfPageTranslationRow>();
            foreach (var row in rows)
            {
                var bounds = Bounds(row, page);
                if (bounds is not { } rect)
                {
                    overflow.Add(row);
                    continue;
                }
                var grouped = new List<PdfPageTranslationRow> { row };
                while (regions.FindLastIndex(region =>
                    region.Bounds.Top + 1 < rect.Bottom && rect.Top + 1 < region.Bounds.Bottom &&
                    region.Bounds.Left < rect.Right && rect.Left < region.Bounds.Right) is var index && index >= 0)
                {
                    var previous = regions[index];
                    grouped.AddRange(previous.Rows);
                    rect = Union(previous.Bounds, rect);
                    regions.RemoveAt(index);
                }
                regions.Add(new(rect, grouped));
            }

            if (rows.Length > 0 && regions.Count == 0)
            {
                canvas.FillRectangle(page);
                OverflowChanged?.Invoke(DrawReflowedPage(canvas, page));
                return;
            }

            foreach (var region in regions)
            {
                var text = new StringBuilder();
                var fragments = new List<Fragment>();
                foreach (var row in region.Rows.OrderBy(row => row.StartUtf16))
                {
                    var isActive = active is not null && row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16;
                    if (text.Length > 0) text.Append(' ');
                    var start = text.Length;
                    text.Append(HasPdfTranslation(row) ? row.Text : "…");
                    fragments.Add(new Fragment(row, start, text.Length, isActive));
                }
                if (!DrawFitted(canvas, text.ToString(), region.Bounds, fragments, gloss))
                    overflow.AddRange(fragments.Select(fragment => fragment.Source));
            }

            OverflowChanged?.Invoke(overflow.OrderBy(row => row.StartUtf16).ToArray());

            bool HasTranslatableText(OcrSpan? span) => span is { } range && rows.Any(row =>
                range.StartUtf16 + offset < row.EndUtf16 && range.EndUtf16 + offset > row.StartUtf16);

            void Mask(double x, double y, double width, double height) =>
                canvas.FillRectangle(page.X + (float)x * page.Width - 1, page.Y + (float)y * page.Height - 1,
                    (float)width * page.Width + 2, (float)height * page.Height + 2);
        }

        private PdfPageTranslationRow[] DrawReflowedPage(ICanvas canvas, RectF page)
        {
            var font = new Microsoft.Maui.Graphics.Font("Georgia");
            var size = Math.Clamp(page.Width * 0.032f, 10, 19);
            var margin = page.Width * 0.075f;
            var width = page.Width - 2 * margin;
            var top = page.Top + margin;
            var bottom = page.Bottom - margin;
            var ordered = rows.OrderBy(row => row.StartUtf16).ToArray();
            for (var index = 0; index < ordered.Length; index++)
            {
                var row = ordered[index];
                var text = HasPdfTranslation(row) ? row.Text! : "…";
                var lines = Wrap(canvas, text, Regex.Matches(text, @"\S+"), font, size, width);
                var height = lines.Count * size * 1.16f;
                if (top + height > bottom) return ordered[index..];
                var isActive = active is not null && row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16;
                var bounds = new RectF(page.Left + margin, top, width, height);
                if (!DrawFitted(canvas, text, bounds, [new(row, 0, text.Length, isActive)], gloss, size))
                    return ordered[index..];
                top += height + size * 0.45f;
            }
            return [];
        }

        private RectF? Bounds(PdfPageTranslationRow row, RectF page)
        {
            RectF? result = null;
            foreach (var line in geometry)
            {
                if (!Intersects(line.SourceSpan)) continue;
                var words = line.Words?.Where(word => Intersects(word.SourceSpan)).ToArray();
                if (words is { Length: > 0 })
                {
                    foreach (var word in words) Include(word.X, word.Y, word.Width, word.Height);
                }
                else Include(line.X, line.Y, line.Width, line.Height);
            }
            return result;

            bool Intersects(OcrSpan? span) => span is { } range &&
                range.StartUtf16 + offset < row.EndUtf16 && range.EndUtf16 + offset > row.StartUtf16;
            void Include(double x, double y, double width, double height)
            {
                var box = new RectF(page.X + (float)x * page.Width, page.Y + (float)y * page.Height,
                    (float)width * page.Width, (float)height * page.Height);
                result = result is { } previous ? Union(previous, box) : box;
            }
        }

        private static RectF Union(RectF a, RectF b)
        {
            var left = Math.Min(a.Left, b.Left);
            var top = Math.Min(a.Top, b.Top);
            return new RectF(left, top, Math.Max(a.Right, b.Right) - left, Math.Max(a.Bottom, b.Bottom) - top);
        }

        private bool DrawFitted(ICanvas canvas, string text, RectF area, IReadOnlyList<Fragment> fragments, string? gloss,
            float? preferredSize = null)
        {
            if (area.Width < 2 || area.Height < 2) return false;
            var font = new Microsoft.Maui.Graphics.Font("Georgia");
            canvas.Font = font;
            canvas.FontColor = Color.FromArgb("#302D27");
            var size = preferredSize ?? Math.Min(21, area.Height * 0.78f);
            var words = Regex.Matches(text, @"\S+");
            if (words.Count == 0) return false;
            const int minimum = 7;
            List<WrappedLine> lines;
            while (true)
            {
                if (size < minimum) return false;
                lines = Wrap(canvas, text, words, font, size, area.Width);
                if (lines.Count * size * 1.16f <= area.Height + 0.01f &&
                    lines.All(line => canvas.GetStringSize(text[line.Start..line.End], font, size).Width <= area.Width))
                    break;
                if (size == minimum) return false;
                size = Math.Max(minimum, size * 0.84f);
            }
            canvas.FontSize = size;
            var y = area.Top;
            foreach (var range in lines)
            {
                if (y + size * 1.16f > area.Bottom + 0.01f) break;
                var line = text[range.Start..range.End];
                foreach (var fragment in fragments)
                {
                    var start = Math.Max(fragment.Start, range.Start) - range.Start;
                    var end = Math.Min(fragment.End, range.End) - range.Start;
                    if (start >= end) continue;
                    var bounds = BoundsFor(start, end);
                    hitTargets.Add((bounds, fragment.Source));
                    if (!fragment.Active) continue;
                    ActiveBounds = ActiveBounds is { } previous ? Union(previous, bounds) : bounds;
                    canvas.FillColor = Color.FromArgb("#FFF1CF");
                    canvas.FillRectangle(bounds);
                    if (!HasPdfTranslation(fragment.Source) || string.IsNullOrWhiteSpace(gloss)) continue;
                    var search = start;
                    while (search <= end - gloss.Length)
                    {
                        var index = line.IndexOf(gloss, search, end - search, StringComparison.OrdinalIgnoreCase);
                        if (index < 0) break;
                        search = index + gloss.Length;
                        if ((index == 0 || !char.IsLetterOrDigit(line[index - 1])) &&
                            (search == line.Length || !char.IsLetterOrDigit(line[search])))
                        {
                            canvas.FillColor = Color.FromArgb("#FFE48B");
                            canvas.FillRectangle(BoundsFor(index, search));
                        }
                    }
                }
                canvas.DrawString(line, area.Left, y, area.Width, size * 1.16f,
                    HorizontalAlignment.Left, VerticalAlignment.Center);
                y += size * 1.16f;

                RectF BoundsFor(int start, int end)
                {
                    var left = canvas.GetStringSize(line[..start], font, size).Width;
                    var width = canvas.GetStringSize(line[start..end], font, size).Width;
                    return new RectF(area.Left + left, y, width, size * 1.16f);
                }
            }
            return true;
        }

        private static List<WrappedLine> Wrap(ICanvas canvas, string text, MatchCollection words,
            Microsoft.Maui.Graphics.Font font, float size, float width)
        {
            var result = new List<WrappedLine>();
            var start = words[0].Index;
            var end = start + words[0].Length;
            for (var index = 1; index < words.Count; index++)
            {
                var word = words[index];
                var next = word.Index + word.Length;
                if (canvas.GetStringSize(text[start..next], font, size).Width > width)
                {
                    result.Add(new WrappedLine(start, end));
                    start = word.Index;
                }
                end = next;
            }
            result.Add(new WrappedLine(start, end));
            return result;
        }
    }
}
