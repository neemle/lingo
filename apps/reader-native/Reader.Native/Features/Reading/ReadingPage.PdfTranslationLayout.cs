using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;
using System.Text;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record PdfLayoutAddress(string Document, int Page, string? Revision, string Target, string? DetachedText)
    {
        public InlinePdfAddress Source => new(Document, Page, Revision, DetachedText);
    }
    private sealed record PdfAutoTranslation(PdfLayoutAddress Address, int RouteRequest);
    private sealed record PdfSourceLayout(InlinePdfAddress Page, OcrLine[] Lines, int Offset, Sentence? Sentence,
        IReadOnlyDictionary<int, PdfBoxAppearance>? Appearance)
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
    private PhotoPreview? pdfTranslationBackgroundPreview;
    private (PdfPageTranslationRow[] Rows, PdfSourceLayout? Source, bool PreserveArtwork, PdfTranslationLayout Plan)? pdfDrawingPlan;
    private PdfSourceLayout? pdfSourceLayout;
    private (PdfLayoutAddress Address, string SentenceId, int Start, int End)? pdfFragmentFocus;
    private Sentence? pdfLayoutSentence => pdfSourceLayout is { } source && source.Matches(pdfLayoutRequest)
        ? PdfHighlightSentence(source.Page, source.Sentence) : null;
    private string? pdfLayoutStatus;
    private int pdfLayoutRequestId;
    private bool refreshingPdfLayout;
    private bool openingPdfTranslationText;
    private PdfLayoutAddress? pendingPdfCacheRefresh;
    private PdfAutoTranslation? pendingPdfAutoTranslation;
    private (PdfAutoTranslation Request, int Attempts)? pdfAutoAttempts;
    private (VerticalStackLayout Panel, PdfPageTranslationRow[] Rows, Label[] Labels, Sentence? Active, string? Query)? overflowContent;
    private (PdfLayoutAddress Address, string SentenceId, RectF Bounds, double ViewportHeight)? lastPdfTranslationFollow;
    private const string UntranslatedPdfFragment = "Фрагмент ще не перекладено";
    private static readonly Color PdfSearchMatchColor = Color.FromArgb("#FFD27A");

    private static bool HasPdfTranslation(PdfPageTranslationRow row) => !string.IsNullOrWhiteSpace(row.Text);
    private static bool NeedsPdfTranslation(PdfPageTranslationRow row) => row.LegacyGroup || !HasPdfTranslation(row);

    private (ScrollView Viewport, GraphicsView Surface) ActivePdfTranslationLayout() =>
        DetachedPdfReader.IsVisible
            ? (DetachedPdfTranslationViewport, DetachedPdfTranslationSurface)
            : (PdfTranslationViewport, PdfTranslationSurface);

    private bool PdfUsesPageTranslation => showInlineOriginal && pdfTranslationLayout && TranslationVisible &&
        SupportsOriginalTranslationLayout(AppState.Session.Document);

    private static bool SupportsOriginalTranslationLayout(Document? document) =>
        document is { Format: "pdf" } or { OcrLines.Length: > 0 };

    private bool PdfPapersSideBySide(bool detached) => PdfUsesPageTranslation &&
        (detached
            ? DetachedPdfReader.IsVisible && DetachedSourcePane.IsVisible && DetachedTargetPane.IsVisible &&
                Grid.GetColumn(DetachedTargetPane) == 1 &&
                DetachedInlinePdfViewport.IsVisible && DetachedPdfTranslationViewport.IsVisible
            : !PdfPageNotice.IsVisible && SourcePane.IsVisible && TranslationPane.IsVisible &&
                Grid.GetColumn(TranslationPane) == 1 &&
                InlinePdfViewport.IsVisible && PdfTranslationViewport.IsVisible);

    private void AlignPdfViewports()
    {
        Align(false, SourcePaneHeader, TranslationPaneHeader, InlinePdfViewport, PdfTranslationViewport,
            TranslationPane);
        Align(true, DetachedSourcePaneHeader, DetachedTranslationPaneHeader, DetachedInlinePdfViewport,
            DetachedPdfTranslationViewport, DetachedTargetPane);

        void Align(bool detached, Grid sourceHeader, Grid targetHeader, ScrollView source,
            ScrollView target, Grid targetPane)
        {
            var aligned = PdfPapersSideBySide(detached);
            sourceHeader.MinimumHeightRequest = aligned ? Math.Max(0, targetHeader.Height) : -1;
            var footer = aligned && target.Height > 0
                ? Math.Max(0, targetPane.Height - target.Y - target.Height) : 0;
            source.Margin = new Thickness(0, 0, 0, footer);
        }
    }

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
        var current = TargetCode is { } target && session.Document is { } document &&
            readinessPair == (document.Language, target) ? readiness : null;
        var directUpgrade = current?.DirectUpgrade is not null;
        var showFooterAction = directUpgrade || current is { Missing.Length: > 0 } && !PdfPageNeedsPackages;
        OpenTranslationPackages.Text = DetachedOpenTranslationPackages.Text = directUpgrade
            ? "Установити прямий переклад" : "Потрібні мовні пакети";
        OpenTranslationPackages.IsVisible = showFooterAction && session.Document is { } source &&
            session.Sentence is { } sentence && !IsFootnote(source, sentence);
        DetachedOpenTranslationPackages.IsVisible = showFooterAction && (directUpgrade || DetachedReadiness.IsVisible);
    }

    private PdfLayoutAddress? CurrentPdfLayoutAddress() =>
        showInlineOriginal && inlinePdfAddress is { } page && TargetCode is { } target &&
        AppState.Session.Document is { } document && SupportsOriginalTranslationLayout(document) &&
        page.Document == document.Id &&
        page.Revision == document.CurrentRevisionId &&
        target != document.Language && (!PdfPageNotice.IsVisible || DetachedPdfReader.IsVisible) &&
        (CurrentNativePdfWindow(document.Id) is not { } window || window.Center == page.Page)
            ? new(page.Document, page.Page, document.CurrentRevisionId, target, page.DetachedText)
            : null;

    private void RefreshPdfTranslationLayout()
    {
        var address = CurrentPdfLayoutAddress();
        var alreadyCached = address == pdfLayoutRequest && pdfLayoutRows.Length > 0 &&
            !pdfLayoutRows.Any(NeedsPdfTranslation);
        AppState.TranslationWorker?.SetVisiblePdf(readingPageActive && TranslationVisible &&
            !alreadyCached && !(autoTranslate && PdfUsesPageTranslation) &&
            address is { Revision: { } revision }
                ? new BackgroundTranslatedPage(address.Document, revision, address.Page, address.Target) : null,
            address?.DetachedText);
        var available = address is not null;
        var detached = DetachedPdfReader.IsVisible;
        var shown = available && pdfTranslationLayout;
        var waitingForPreview = PdfUsesPageTranslation && inlinePdfAddress is null && inlinePdfLoading is not null;
        if (translatingPdfLayout is not null && (!shown || !TranslationVisible || !readingPageActive ||
            translatingPdfLayout != address))
            pdfLayoutTranslationCancellation?.Cancel();
        PdfTranslationMode.IsVisible = available && !detached;
        DetachedPdfTranslationMode.IsVisible = available && detached;
        PdfTranslationMode.Text = DetachedPdfTranslationMode.Text = shown ? "Список" : "Верстка";
        PdfTranslationViewport.IsVisible = TranslatePdfPage.IsVisible = shown && !detached;
        DetachedPdfTranslationViewport.IsVisible = DetachedTranslatePdfPage.IsVisible = shown && detached;
        TranslationViewport.IsVisible = !waitingForPreview && !PdfTranslationViewport.IsVisible;
        DetachedTranslationViewport.IsVisible = !waitingForPreview && !DetachedPdfTranslationViewport.IsVisible;
        var translatingPage = translatingPdfLayout == address && address is not null;
        if (address != pdfLayoutRequest)
        {
            pdfLayoutRequest = address;
            pdfFragmentFocus = null;
            pendingPdfCacheRefresh = null;
            pdfLayoutRequestId++;
            pdfAutoAttempts = null;
            pdfLayoutRows = [];
            pdfDrawingPlan = null;
            PdfTranslationSurface.Drawable = DetachedPdfTranslationSurface.Drawable = null;
            PdfTranslationBackground.Source = DetachedPdfTranslationBackground.Source = null;
            pdfTranslationBackgroundPreview = null;
            PdfTranslationSurface.Invalidate();
            DetachedPdfTranslationSurface.Invalidate();
            pdfLayoutStatus = null;
            overflowContent = null;
            lastPdfTranslationFollow = null;
            PdfTranslationOverflow.IsVisible = DetachedPdfTranslationOverflow.IsVisible = false;
            if (shown && address is not null) _ = LoadPdfPageLayoutAsync(address, cachedOnly: true);
        }
        else if (shown && address is not null && pdfLayoutRows.Length == 0 && pdfLayoutStatus is null)
            _ = LoadPdfPageLayoutAsync(address, cachedOnly: true);
        UpdatePdfTranslationStatus(shown ? address : null, detached, translatingPage);
        AlignPdfViewports();
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
        var photo = AppState.Session.Document is { Format: not "pdf" };
        TranslatePdfPage.Text = DetachedTranslatePdfPage.Text = translatingPage ? "Перекладаємо…"
            : needsPackages ? "Мовні пакети" : photo ? "Перекласти фото" : "Перекласти сторінку";
        string? text = null;
        if (address is not null)
        {
            if (translatingPage) text = photo ? "Перекладаємо фото…" : "Перекладаємо сторінку…";
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
                text = photo ? "Фото очікує перекладу." : "Сторінка очікує перекладу.";
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
            readinessPair != (document.Language, address.Target) || !pdfLayoutRows.Any(NeedsPdfTranslation))
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
        var query = PdfTranslationFindQuery();
        foreach (var row in rows)
        {
            var label = PdfTranslationTextLabel(row, 20, query);
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
        var page = session.Document?.Format == "pdf"
            ? await session.PdfReadingPageAsync(address.Document) ?? session.OriginalPageIndex() : 0;
        if (page != address.Page || !IsCurrentPdfTranslationSource(address)) return;

        if (readingLayout == ReaderLayout.Translation)
        {
            readingLayout = ReaderLayout.Automatic;
            ArrangeReadingColumns();
        }
        await SelectTranslatedPdfRowAsync(fragment);
    }

    private bool IsCurrentPdfTranslationSource(PdfLayoutAddress address) =>
        readingPageActive && !AppState.Session.Busy && TargetCode == address.Target &&
        AppState.Session.Document is { Format: "pdf" } document &&
        document.Id == address.Document && document.CurrentRevisionId == address.Revision;

    private Label PdfTranslationTextLabel(PdfPageTranslationRow row, double size, string? query = null)
    {
        if (row.Text is not { } text || !HasPdfTranslation(row))
            return new Label { Text = UntranslatedPdfFragment, Style = (Style)Resources["ReaderMeta"] };
        var label = new Label { FontFamily = "Georgia", FontSize = size, LineHeight = 1.3 };
        if (pdfLayoutSentence is { } active && row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16 &&
            query is not null && DocumentFind.SourceMatch(text, query) is { } match)
        {
            var end = match.StartUtf16 + match.LengthUtf16;
            label.FormattedText = new FormattedString
            {
                Spans =
                {
                    new Span { Text = text[..match.StartUtf16] },
                    new Span { Text = match.Text, BackgroundColor = PdfSearchMatchColor },
                    new Span { Text = text[end..] },
                },
            };
        }
        else label.Text = text;
        return label;
    }

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
            (pdfLayoutRows.Length == 0 || pdfLayoutRows.Any(NeedsPdfTranslation));
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
        if (TranslationPaneHeader is null || DetachedTranslationPaneHeader is null || !showInlineOriginal) return;
        AlignPdfViewports();
        ResizeInlinePdf();
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
        var (viewport, _) = ActivePdfTranslationLayout();
        if (!viewport.IsVisible) return;
        var paper = DetachedPdfReader.IsVisible ? DetachedPdfTranslationPaper : PdfTranslationPaper;
        ResizePdfPaper(viewport, paper);
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
        await SelectPdfSourceRangeAsync(address.Source, fragment.StartUtf16, fragment.EndUtf16, fragment.SourceSentenceId, address);
    }

    private Sentence? PdfHighlightSentence(InlinePdfAddress page, Sentence? sentence)
    {
        if (sentence is not null && pdfFragmentFocus is { } focus &&
            focus.Address == pdfLayoutRequest && focus.Address.Source == page && focus.SentenceId == sentence.Id)
            return sentence with { StartUtf16 = focus.Start, EndUtf16 = focus.End };
        if (sentence is not null && documentFindFocus is { PdfFragment: { } fragment } found &&
            found.Document == page.Document && found.Target == TargetCode &&
            found.Sentence == sentence.Id && fragment.Page == page.Page &&
            (page.DetachedText is null
                ? found.Page is null && found.Revision == page.Revision
                : found.Page == page.Page && found.Revision == detachedSentence?.RevisionId))
            return sentence with { StartUtf16 = fragment.StartUtf16, EndUtf16 = fragment.EndUtf16 };
        return sentence;
    }

    private void ClearPdfFragmentFocus()
    {
        var foundFragment = documentFindFocus?.PdfFragment is not null;
        if (pdfFragmentFocus is null && !foundFragment) return;
        pdfFragmentFocus = null;
        if (!foundFragment)
        {
            UpdateInlinePdfHighlight();
            return;
        }
        documentFindFocus = null;
        if (DetachedPdfReader.IsVisible && detachedSentence is { } page)
        {
            DetachedPdfText.HighlightStart = page.Sentence.StartUtf16;
            DetachedPdfText.HighlightLength = page.Sentence.EndUtf16 - page.Sentence.StartUtf16;
            UpdateInlinePdfHighlight();
        }
        else UpdateHighlight();
        RefreshFindTranslationDisplay();
    }

    private async Task SelectPdfSourceRangeAsync(InlinePdfAddress address, int start, int end, string? sentenceId = null,
        PdfLayoutAddress? translated = null)
    {
        var session = AppState.Session;
        if (!readingPageActive || session.Busy) return;
        lastInlinePdfFollow = null;
        pdfFragmentFocus = null;
        if (address.DetachedText is not null)
        {
            if (detachedAddress is { } detached && detached.Document == address.Document &&
                detached.Page == address.Page && detached.Text == address.DetachedText && !loadingDetachedSentence)
            {
                await LoadDetachedSentenceAsync(detached, position: null, sentenceId: sentenceId, sourceOffset: start);
                if (FocusTranslatedFragment(detachedSentence?.Sentence)) UpdateInlinePdfHighlight();
            }
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
        if (position != document.Position || (translated is not null && utteranceActive)) StopSpeech();
        if (position != document.Position)
            await session.SelectSentenceAsync(position, pdfPage: document.Format == "pdf" ? address.Page : null);
        if (session.Error.Length == 0 && session.Sentence?.Id == document.Sentences[position].Id)
        {
            FocusTranslatedFragment(session.Sentence);
            UpdateHighlight();
            SourceText.RevealOffset(start);
        }

        bool FocusTranslatedFragment(Sentence? sentence)
        {
            if (!readingPageActive || translated is null || CurrentPdfLayoutAddress() != translated ||
                sentence is null || sentence.Id != sentenceId) return false;
            pdfFragmentFocus = (translated, sentence.Id, start, end);
            return true;
        }
    }

    private async Task FollowPdfTranslationScrollAsync(bool fromOriginal)
    {
        var (original, originalSurface) = ActiveInlinePdf();
        var (translated, translatedSurface) = ActivePdfTranslationLayout();
        if (!PdfFollowsScroll || syncingPdfTranslationScroll || !original.IsVisible || !translated.IsVisible ||
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
        if (!ReferenceEquals(pdfTranslationBackgroundPreview, inlinePdfPreview))
        {
            pdfTranslationBackgroundPreview = inlinePdfPreview;
            PdfTranslationBackground.Source = DetachedPdfTranslationBackground.Source = null;
        }
        if (background.Source is null && inlinePdfPreview is { } preview)
            background.Source = InlinePreviewSource(preview);
        var source = pdfSourceLayout is { } layout && layout.Matches(pdfLayoutRequest) ? layout : null;
        background.IsVisible = background.Source is not null && source is { Lines.Length: > 0 } &&
            pdfLayoutRows.Length > 0;
        var gloss = (detached ? DetachedWordCard : WordCard).IsVisible ? PopupGloss(detached) : null;
        QueueGlossAlignment(gloss, pdfLayoutRows.Select(row => row.Text));
        var query = PdfTranslationFindQuery();
        if (pdfDrawingPlan is not { } cached || cached.Rows != pdfLayoutRows || !SamePdfGeometry(cached.Source, source) ||
            cached.PreserveArtwork != background.IsVisible)
        {
            cached = (pdfLayoutRows, source, background.IsVisible,
                PlanPdfDrawing(pdfLayoutRows, source, background.IsVisible));
            pdfDrawingPlan = cached;
        }
        var drawing = new TranslatedPdfDrawing(pdfLayoutRows,
            pdfLayoutSentence, text => GlossRanges(text, gloss), background.IsVisible, query,
            PdfReadingHorizontalBounds(), cached.Plan, surface);
        var overflow = detached ? DetachedPdfTranslationOverflow : PdfTranslationOverflow;
        drawing.OverflowChanged = rows => Dispatcher.Dispatch(() =>
        {
            if (viewport.IsVisible && ReferenceEquals(surface.Drawable, drawing))
            {
                var description = string.Join(" ", pdfLayoutRows.Except(rows)
                    .Select(row => HasPdfTranslation(row) ? row.Text : UntranslatedPdfFragment));
                if (SemanticProperties.GetDescription(surface) != description)
                    SemanticProperties.SetDescription(surface, description);
                var bounds = drawing.MatchBounds ?? drawing.ActiveBounds;
                ShowPdfTranslationOverflow(overflow, rows, bounds is not null, query);
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
        if (!PdfFollowsScroll || syncingPdfTranslationScroll || viewport.Height <= 0 ||
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

    private void ShowPdfTranslationOverflow(VerticalStackLayout panel, PdfPageTranslationRow[] rows, bool hasRenderedActive,
        string? query)
    {
        var active = pdfLayoutSentence;
        var previous = overflowContent;
        Label[] labels;
        if (previous is { } current && current.Panel == panel && current.Query == query &&
            (query is null || current.Active == active) && current.Rows.SequenceEqual(rows))
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
                    var label = PdfTranslationTextLabel(row, 19, query);
                    var tap = new TapGestureRecognizer();
                    tap.Tapped += async (_, _) => await SelectTranslatedPdfRowAsync(row);
                    label.GestureRecognizers.Add(tap);
                    panel.Children.Add(label);
                    labels[index] = label;
                }
            }
        }
        overflowContent = (panel, rows, labels, active, query);

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
            (previous?.Active != active || previous?.Panel != panel || previous?.Query != query ||
                previouslyInOverflow != (activeLabel is not null)))
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
                if (PdfFollowsScroll)
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

    private static bool SamePdfGeometry(PdfSourceLayout? first, PdfSourceLayout? second) =>
        ReferenceEquals(first, second) || first is not null && second is not null &&
        first.Page == second.Page && first.Offset == second.Offset &&
        ReferenceEquals(first.Appearance, second.Appearance) && first.Lines.SequenceEqual(second.Lines);

    private static PdfTranslationLayout PlanPdfDrawing(PdfPageTranslationRow[] rows, PdfSourceLayout? source,
        bool preserveArtwork)
    {
        var spans = Array.ConvertAll(rows, row => new PdfTranslationSourceRow(
            row.StartUtf16, row.EndUtf16, row.SourceBlockIndex, preserveArtwork && !HasPdfTranslation(row)));
        var styles = source?.Appearance?.ToDictionary(entry => entry.Key, entry =>
        {
            var style = entry.Value;
            return new PdfTranslationSourceStyle(style.FontFamily, style.RelativeFontSize, style.Weight,
                style.Italic, style.Ink is { } ink ? Rgba(ink) : null, Rgba(style.Background));
        }) ?? [];
        return CoreClient.CoreClient.PlanPdfTranslationLayout(spans, source?.Lines ?? [], source?.Offset ?? 0,
            styles, preserveArtwork);

        static double[] Rgba(Color color) => [color.Red, color.Green, color.Blue, color.Alpha];
    }

    private sealed class TranslatedPdfDrawing(PdfPageTranslationRow[] rows,
        Sentence? active, Func<string, IEnumerable<(int Start, int End)>> glossRanges, bool preserveArtwork, string? query,
        (double Left, double Right) visibleWidth, PdfTranslationLayout layout, GraphicsView surface) : IDrawable
    {
        private readonly record struct Fragment(PdfPageTranslationRow Source, int Start, int End, bool Active, TextSelection? Match);
        private readonly List<(RectF Bounds, PdfPageTranslationRow Source)> hitTargets = [];
        internal Action<PdfPageTranslationRow[]>? OverflowChanged { get; set; }
        internal RectF? ActiveBounds { get; private set; }
        internal RectF? MatchBounds { get; private set; }

        internal double? SourceTop(PdfPageTranslationRow row)
        {
            var index = Array.IndexOf(rows, row);
            return index >= 0 ? layout.Anchors[index].Bounds?.Y : null;
        }

        internal PdfPageTranslationRow? HitTest(Point point)
        {
            foreach (var (bounds, source) in hitTargets)
                if (point.X >= bounds.Left && point.X <= bounds.Right &&
                    point.Y >= bounds.Top && point.Y <= bounds.Bottom)
                    return source;
            return null;
        }

        public void Draw(ICanvas canvas, RectF dirtyRect)
        {
            hitTargets.Clear();
            ActiveBounds = null;
            MatchBounds = null;
            var page = surface.Width > 0 && surface.Height > 0
                ? new RectF(0, 0, (float)surface.Width, (float)surface.Height) : dirtyRect;
            canvas.FillColor = Colors.White;
            if (page.Width <= 0 || page.Height <= 0) return;
            if (preserveArtwork)
            {
                canvas.BlendMode = BlendMode.Clear;
                canvas.FillRectangle(page);
                canvas.BlendMode = BlendMode.Normal;
            }
            else canvas.FillRectangle(page);
            var overflow = layout.Overflow.Select(index => rows[index]).ToList();
            if (rows.Length > 0 && layout.Regions.Length == 0 && layout.Pending.Length == 0 &&
                (!preserveArtwork || layout.Anchors.All(fragment => fragment.Bounds is null)))
            {
                canvas.FillRectangle(page);
                var reflowBounds = new RectF(page.X + (float)visibleWidth.Left * page.Width, page.Y,
                    (float)(visibleWidth.Right - visibleWidth.Left) * page.Width, page.Height);
                var reflowOverflow = DrawReflowedPage(canvas, reflowBounds);
                OverflowChanged?.Invoke(reflowOverflow);
                return;
            }

            var foreground = new List<Action>();
            foreach (var region in layout.Regions)
            {
                var regionRows = Array.ConvertAll(region.Fragments, index => rows[index]);
                var appearance = region.StyleFragment is { } style ? TranslateAppearance(layout.Anchors[style].Style) : null;
                var text = new StringBuilder();
                var fragments = new List<Fragment>();
                foreach (var row in regionRows)
                {
                    var isActive = active is not null && row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16;
                    if (text.Length > 0) text.Append(' ');
                    var start = text.Length;
                    text.Append(HasPdfTranslation(row) ? row.Text : "…");
                    fragments.Add(MakeFragment(row, start, text.Length, isActive));
                }
                var preferredSize = (float)(page.Height * region.PreferredFontHeight);
                if (region.LimitFontByWidth) preferredSize = Math.Min(page.Width * 0.045f, preferredSize);
                var minimumSize = Math.Max(2, preferredSize * 0.60f);
                var area = Scale(region.Bounds, page);
                var translated = text.ToString();
                var font = new Microsoft.Maui.Graphics.Font(appearance?.FontFamily ?? "Georgia", appearance?.Weight ?? 400,
                    appearance?.Italic == true ? FontStyleType.Italic : FontStyleType.Normal);
                var fit = area.Width >= 2 && area.Height >= 2
                    ? FitText(canvas, translated, font, area.Width, area.Height, preferredSize, minimumSize) : null;
                if (fit is null)
                {
                    overflow.AddRange(fragments.Select(fragment => fragment.Source));
                    continue;
                }
                foreach (var mask in region.Masks)
                {
                    canvas.FillColor = ColorFrom(mask.Background);
                    canvas.FillRectangle(Scale(mask.Bounds, page));
                }
                var ink = appearance?.Ink ?? Color.FromArgb("#302D27");
                foreground.Add(() => DrawText(canvas, translated, area, fragments, font, ink, fit, region.Alignment));
            }
            foreach (var paint in foreground) paint();

            foreach (var index in layout.Pending)
            {
                var row = rows[index];
                var bounds = Scale(layout.Anchors[index].Bounds!, page);
                // Keep the original pixels visible and selectable until this row has a translation.
                hitTargets.Add((bounds, row));
                if (active is null || row.StartUtf16 >= active.EndUtf16 || row.EndUtf16 <= active.StartUtf16)
                    continue;
                ActiveBounds = ActiveBounds is { } previous ? Union(previous, bounds) : bounds;
                canvas.FillColor = Color.FromRgba(255, 212, 122, 64);
                canvas.FillRectangle(bounds);
            }

            OverflowChanged?.Invoke(overflow.OrderBy(row => row.StartUtf16).ToArray());
        }

        private static RectF Scale(PdfSampleBox bounds, RectF page) =>
            new(page.X + (float)bounds.X * page.Width, page.Y + (float)bounds.Y * page.Height,
                (float)bounds.Width * page.Width, (float)bounds.Height * page.Height);

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
                var layout = FitText(canvas, text, font, width, bottom - top, size, size);
                if (layout is null) return ordered[index..];
                var height = layout.Lines.Length * layout.LineHeight;
                var isActive = active is not null && row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16;
                var bounds = new RectF(page.Left + margin, top, width, height);
                DrawText(canvas, text, bounds, [MakeFragment(row, 0, text.Length, isActive)],
                    font, Color.FromArgb("#302D27"), layout);
                top += height + size * 0.45f;
            }
            return [];
        }

        private Fragment MakeFragment(PdfPageTranslationRow row, int start, int end, bool isActive) =>
            new(row, start, end, isActive, isActive && query is not null && row.Text is { } text
                ? DocumentFind.SourceMatch(text, query, start) : null);

        private static PdfBoxAppearance? TranslateAppearance(PdfTranslationSourceStyle? style) => style is null
            ? null
            : new(style.FontFamily, style.FontHeight ?? 0, style.Weight, style.Italic,
                style.Ink is { } ink ? ColorFrom(ink) : null, ColorFrom(style.Background));

        private static Color ColorFrom(double[] channels) =>
            new((float)channels[0], (float)channels[1], (float)channels[2], (float)channels[3]);

        private static RectF Union(RectF a, RectF b)
        {
            var left = Math.Min(a.Left, b.Left);
            var top = Math.Min(a.Top, b.Top);
            return new RectF(left, top, Math.Max(a.Right, b.Right) - left, Math.Max(a.Bottom, b.Bottom) - top);
        }

        private static PdfTextFit? FitText(ICanvas canvas, string text, Microsoft.Maui.Graphics.Font font,
            float width, float height, float preferredSize, float minimumSize) =>
            CoreClient.CoreClient.FitPdfText(text, width, height, preferredSize, minimumSize,
                (start, length, size) => length == 0
                    ? canvas.GetStringSize("Ág", font, size).Height
                    : canvas.GetStringSize(text.Substring(start, length), font, size).Width);

        private void DrawText(ICanvas canvas, string text, RectF area, IReadOnlyList<Fragment> fragments,
            Microsoft.Maui.Graphics.Font font, Color ink, PdfTextFit layout, string alignment = "left")
        {
            var lightInk = ink.Red * 0.21f + ink.Green * 0.72f + ink.Blue * 0.07f > 0.55f;
            var size = layout.Size;
            var lineHeight = layout.LineHeight;
            canvas.Font = font;
            canvas.FontColor = ink;
            canvas.FontSize = size;
            canvas.SaveState();
            canvas.ClipRectangle(area.X, area.Y, area.Width, area.Height);
            var y = area.Top;
            foreach (var range in layout.Lines)
            {
                var line = text[range.Start..range.End];
                var x = area.Left;
                if (alignment is "center" or "right")
                {
                    var slack = Math.Max(0, area.Width - canvas.GetStringSize(line, font, size).Width);
                    x += alignment == "center" ? slack * 0.5f : slack;
                }
                foreach (var fragment in fragments)
                {
                    var start = Math.Max(fragment.Start, range.Start) - range.Start;
                    var end = Math.Min(fragment.End, range.End) - range.Start;
                    if (start >= end) continue;
                    var bounds = BoundsFor(start, end);
                    hitTargets.Add((bounds, fragment.Source));
                    if (!fragment.Active) continue;
                    ActiveBounds = ActiveBounds is { } previous ? Union(previous, bounds) : bounds;
                    canvas.FillColor = Color.FromArgb(lightInk ? "#4A391E" : "#FFF1CF");
                    canvas.FillRectangle(bounds);
                    if (fragment.Match is { } match)
                    {
                        var matchStart = Math.Max(match.StartUtf16 - range.Start, start);
                        var matchEnd = Math.Min(match.StartUtf16 + match.LengthUtf16 - range.Start, end);
                        if (matchStart < matchEnd)
                        {
                            var matchBounds = BoundsFor(matchStart, matchEnd);
                            MatchBounds = MatchBounds is { } previousMatch ? Union(previousMatch, matchBounds) : matchBounds;
                            canvas.FillColor = lightInk ? Color.FromArgb("#604321") : PdfSearchMatchColor;
                            canvas.FillRectangle(matchBounds);
                        }
                    }
                    if (fragment.Source.Text is not { } translation) continue;
                    foreach (var (glossStart, glossEnd) in glossRanges(translation))
                    {
                        var left = Math.Max(fragment.Start + glossStart - range.Start, start);
                        var right = Math.Min(fragment.Start + glossEnd - range.Start, end);
                        if (left < right)
                        {
                            canvas.FillColor = Color.FromArgb(lightInk ? "#604321" : "#FFE48B");
                            canvas.FillRectangle(BoundsFor(left, right));
                        }
                    }
                }
                // Core Text can reject a line whose frame is exactly its measured height.
                // Give the platform room for font leading and clip to the source region.
                canvas.DrawString(line, x, y, area.Right - x + 2, lineHeight + Math.Max(2, size * 0.5f),
                    HorizontalAlignment.Left, VerticalAlignment.Top, TextFlow.OverflowBounds);
                y += lineHeight;

                RectF BoundsFor(int start, int end)
                {
                    var left = canvas.GetStringSize(line[..start], font, size).Width;
                    var width = canvas.GetStringSize(line[start..end], font, size).Width;
                    return new RectF(x + left, y, width, lineHeight);
                }
            }
            canvas.RestoreState();
        }
    }
}
