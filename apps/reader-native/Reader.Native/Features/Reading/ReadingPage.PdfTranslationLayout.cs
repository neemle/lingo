using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;
using System.Text;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record PdfLayoutAddress(string Document, int Page, string? Revision, string SourceLanguage,
        string Target, string? DetachedText)
    {
        public InlinePdfAddress Source => new(Document, Page, Revision, DetachedText);

        public bool MatchesSource(Document? document, string? target, PdfLayoutAddress? displayed) =>
            document is not null && document.Id == Document && document.CurrentRevisionId == Revision &&
            document.Language == SourceLanguage && target == Target &&
            (displayed is null || displayed.Page != Page || displayed.DetachedText == DetachedText);

        public bool DependsOnSentenceRefresh(PdfLayoutAddress refreshed, Sentence sentence, PdfPageTranslationRow[] rows) =>
            this == refreshed || (SharesCanonicalSource(refreshed) &&
                rows.Any(row => row.SourceSentenceId == sentence.Id || IsActivePdfRow(row, sentence)));

        public bool SharesCanonicalSource(PdfLayoutAddress other) =>
            DetachedText is null && other.DetachedText is null && (this with { Page = other.Page }) == other;
    }
    private sealed record PdfLayoutSentenceRefresh(Sentence Source, PdfSentenceRefresh Request);
    private sealed record PdfAutoTranslation(PdfLayoutAddress Address, int RouteRequest);
    private sealed record PdfSourceLayout(InlinePdfAddress Page, OcrLine[] Lines, int Offset, Sentence? Sentence,
        IReadOnlyDictionary<int, PdfBoxAppearance>? Appearance)
    {
        public bool Matches(PdfLayoutAddress? address) => address is not null &&
            Page.Document == address.Document && Page.Page == address.Page &&
            Page.Revision == address.Revision && Page.DetachedText == address.DetachedText;
    }
    private bool pdfTranslationLayout = true;
    private TaskCompletionSource? pdfTranslationScroll;
    private PdfLayoutAddress? pdfLayoutRequest;
    private PdfLayoutAddress? translatingPdfLayout;
    private bool keepPdfTranslationOnNavigation;
    private bool refreshingPdfSentence;
    private CancellationTokenSource? pdfLayoutTranslationCancellation;
    private const int RecentPdfTranslationLimit = 3;
    private readonly List<(PdfLayoutAddress Address, PdfPageTranslationRow[] Rows)> recentPdfTranslations = [];
    private PdfPageTranslationRow[] pdfLayoutRows = [];
    private bool pdfLayoutRowsStale;
    private PhotoPreview? pdfTranslationBackgroundPreview;
    private (PdfPageTranslationRow[] Rows, PdfSourceLayout? Source, bool PreserveArtwork, PdfTranslationLayout Plan)? pdfDrawingPlan;
    private PdfSourceLayout? pdfSourceLayout;
    private (PdfLayoutAddress Address, string SentenceId, int Start, int End)? pdfFragmentFocus;
    private Sentence? pdfLayoutSentence => pdfSourceLayout is { } source && source.Matches(pdfLayoutRequest)
        ? PdfHighlightSentence(source.Page, source.Sentence) : null;
    private string? pdfLayoutStatus;
    private int pdfLayoutRequestId;
    private int pdfTranslationViewportRequest;
    private int pdfTranslationSelectionRequest;
    private bool refreshingPdfLayout;
    private bool openingPdfTranslationText;
    private PdfLayoutAddress? pendingPdfCacheRefresh;
    private PdfAutoTranslation? pendingPdfAutoTranslation;
    private (PdfAutoTranslation Request, int Attempts)? pdfAutoAttempts;
    private (VerticalStackLayout Panel, PdfPageTranslationRow[] Rows, Label[] Labels, Sentence? Active,
        string? Query, string? Gloss, int GlossResults)? overflowContent;
    private (PdfLayoutAddress Address, string SentenceId, RectF Bounds, double ViewportHeight)? lastPdfTranslationFollow;
    private const string UntranslatedPdfFragment = "Фрагмент ще не перекладено";
    private static string PdfFragmentState(PdfPageTranslationRow row) =>
        row.Failure is null ? "Не перекладено" : "Помилка перекладу";
    private static readonly Color TranslationSearchMatchColor = Color.FromArgb("#FFD27A");

    private static bool HasPdfTranslation(PdfPageTranslationRow row) => !string.IsNullOrWhiteSpace(row.Text);
    private static bool NeedsPdfTranslation(PdfPageTranslationRow row) => row.LegacyGroup || !HasPdfTranslation(row);
    private bool NeedsPreferredPdfTranslation(PdfPageTranslationRow row) =>
        NeedsPdfTranslation(row) || (!row.Reviewed && readiness is { Ready: true, DirectModelRevision: { } revision }
            && (row.PivotLanguage is not null || row.DirectModelRevision is { } previous && previous != revision));

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
        if (pdfTranslationLayout && pdfLayoutRowsStale) RefreshCachedPdfPageLayout();
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
            await LoadPdfPageLayoutAsync(address, cachedOnly: false, userRequested: true);
    }

    private async void RefreshSentenceTranslationClicked(object? sender, EventArgs args)
    {
        if (sender is not Button { IsEnabled: true }) return;
        if (PdfUsesPageTranslation)
        {
            if (pdfLayoutRequest is { } address && pdfLayoutSentence is { } sentence)
            {
                var row = pdfLayoutRows.FirstOrDefault(value =>
                    value.StartUtf16 < sentence.EndUtf16 && value.EndUtf16 > sentence.StartUtf16);
                var offset = Math.Max(sentence.StartUtf16, row?.StartUtf16 ?? sentence.StartUtf16);
                var revision = address.DetachedText is null ? address.Revision : detachedSentence?.RevisionId;
                var refresh = new PdfSentenceRefresh(offset, new(sentence.Id, address.SourceLanguage, revision));
                await LoadPdfPageLayoutAsync(address, cachedOnly: false, refresh: new(sentence, refresh));
            }
        }
        else if (DetachedPdfReader.IsVisible)
            await RefreshDetachedSentenceTranslationAsync();
        else
        {
            lastAutoTranslation = ActiveTranslationKey();
            await TranslateOfflineAsync(useContext: false, regenerate: true);
        }
    }

    private void UpdateSentenceRefreshButtons()
    {
        var session = AppState.Session;
        var ready = readingPageActive && TranslationVisible && !session.Busy && !translating &&
            !translatingDetachedSentence && translatingPdfLayout is null && readiness?.Ready == true;
        var detached = DetachedPdfReader.IsVisible;
        var hasSource = !PdfUsesPageTranslation || pdfLayoutSentence is not null && pdfLayoutRequest is not null;
        RefreshSentenceTranslation.IsEnabled = ready && hasSource && !detached &&
            ActiveTranslationKey() is { } key && readinessPair == (key.Source, key.Target) &&
            session.Document is { } document && session.Sentence is { } sentence && !IsFootnote(document, sentence) &&
            session.ReviewedTranslation(key.Target) is null;
        DetachedRefreshSentenceTranslation.IsEnabled = ready && hasSource && detached && !loadingDetachedSentence &&
            detachedSentence is { Footnote: false } current && TargetCode is { } target &&
            readinessPair == (current.Language, target) && ReviewedDetachedTranslation() is null;
    }

    private static void LabelTranslationIcon(Button button, string glyph, string label)
    {
        button.Text = glyph;
        DescribeButton(button, label);
    }

    internal static void DescribeButton(Button button, string label)
    {
        if (ToolTipProperties.GetText(button) as string != label)
        {
            ToolTipProperties.SetText(button, label);
#if MACCATALYST
            if (button.Handler?.PlatformView is UIKit.UIButton platformButton)
            {
                // Use the control's native tooltip setter so Catalyst refreshes
                // the hover text when the action changes.
                var builtIn = platformButton.ToolTipInteraction;
                foreach (var tooltip in platformButton.Interactions.OfType<UIKit.UIToolTipInteraction>())
                    if (tooltip.Handle != builtIn?.Handle)
                        platformButton.RemoveInteraction(tooltip);
                platformButton.ToolTip = label;
            }
#endif
        }
        SemanticProperties.SetDescription(button, label);
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
        DetachedPdfReader.IsVisible == (page.DetachedText is not null) &&
        (!DetachedPdfReader.IsVisible || detachedAddress?.Text == page.DetachedText) &&
        target != document.Language && (!PdfPageNotice.IsVisible || DetachedPdfReader.IsVisible) &&
        (CurrentNativePdfWindow(document.Id) is not { } window || window.Center == page.Page)
            ? new(page.Document, page.Page, document.CurrentRevisionId, document.Language, target, page.DetachedText)
            : null;

    private (PdfLayoutAddress Address, int Offset)? ReflowPdfRefreshTarget(Document document, Sentence sentence, string target)
    {
        if (!SupportsOriginalTranslationLayout(document)) return null;
        var page = 0;
        var offset = sentence.StartUtf16;
        if (document.Format == "pdf")
        {
            page = CurrentNativePdfWindow(document.Id)?.Center ?? AppState.Session.OriginalPageIndex();
            if (document.PdfPageStarts is not { } starts || page < 0 || page >= starts.Length ||
                starts[page] is not { } start) return null;
            var end = document.Text.Length;
            for (var next = page + 1; next < starts.Length; next++)
            {
                if (starts[next] is not { } boundary) continue;
                end = boundary;
                break;
            }
            offset = Math.Max(offset, start);
            if (offset >= end || offset >= sentence.EndUtf16) return null;
        }
        return (new PdfLayoutAddress(document.Id, page, document.CurrentRevisionId, document.Language, target, null), offset);
    }

    private void RefreshPdfTranslationLayout()
    {
        var address = CurrentPdfLayoutAddress();
        var alreadyCached = address == pdfLayoutRequest && pdfLayoutRows.Length > 0 &&
            !pdfLayoutRows.Any(NeedsPreferredPdfTranslation);
        AppState.TranslationWorker?.SetVisiblePdf(readingPageActive && TranslationVisible &&
            !alreadyCached && !(autoTranslate && PdfUsesPageTranslation) &&
            address is { Revision: { } revision }
                ? new BackgroundTranslatedPage(address.Document, revision, address.Page, address.SourceLanguage, address.Target) : null,
            address?.DetachedText);
        var available = address is not null;
        var detached = DetachedPdfReader.IsVisible;
        var shown = available && pdfTranslationLayout;
        var waitingForPreview = PdfUsesPageTranslation && inlinePdfAddress is null && inlinePdfLoading is not null;
        if (translatingPdfLayout is { } running)
        {
            var keepRunning = readingPageActive && (keepPdfTranslationOnNavigation
                ? running.MatchesSource(AppState.Session.Document, TargetCode, address)
                : shown && TranslationVisible && running == address);
            if (!keepRunning) ReleasePdfLayoutTranslation()?.Cancel();
        }
        PdfTranslationMode.IsVisible = available && !detached;
        DetachedPdfTranslationMode.IsVisible = available && detached;
        var modeGlyph = shown ? "\uf03a" : "\uf0db";
        var modeLabel = shown ? "Показати переклад списком" : "Показати переклад у верстці сторінки";
        LabelTranslationIcon(PdfTranslationMode, modeGlyph, modeLabel);
        LabelTranslationIcon(DetachedPdfTranslationMode, modeGlyph, modeLabel);
        PdfTranslationViewport.IsVisible = TranslatePdfPage.IsVisible = shown && !detached;
        DetachedPdfTranslationViewport.IsVisible = DetachedTranslatePdfPage.IsVisible = shown && detached;
        TranslationViewport.IsVisible = !waitingForPreview && !PdfTranslationViewport.IsVisible;
        DetachedTranslationViewport.IsVisible = !waitingForPreview && !DetachedPdfTranslationViewport.IsVisible;
        var translatingPage = translatingPdfLayout == address && address is not null;
        if (address != pdfLayoutRequest)
        {
            if (!pdfLayoutRowsStale && pdfLayoutRequest is { } previous)
                RememberPdfPageTranslations(previous, pdfLayoutRows);
            pdfLayoutRequest = address;
            pdfTranslationSelectionRequest++;
            pdfFragmentFocus = null;
            pendingPdfCacheRefresh = null;
            pdfLayoutRequestId++;
            pdfAutoAttempts = null;
            pdfLayoutRows = address is not null
                ? recentPdfTranslations.Find(entry => entry.Address == address).Rows ?? []
                : [];
            pdfLayoutRowsStale = false;
            pdfDrawingPlan = null;
            PdfTranslationSurface.Drawable = DetachedPdfTranslationSurface.Drawable = null;
            ClearPdfTranslationBackground();
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
        UpdateSentenceRefreshButtons();
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
        else ClearPdfTranslationBackground();
        SchedulePdfAutoTranslation();
    }

    private void UpdatePdfTranslationStatus(PdfLayoutAddress? address, bool detached, bool translatingPage)
    {
        var ready = address is not null && PdfPageRouteReady(address);
        var needsPackages = address is not null && PdfPageNeedsPackages;
        TranslatePdfPage.IsEnabled = DetachedTranslatePdfPage.IsEnabled =
            (ready || needsPackages) && translatingPdfLayout is null && !AppState.Session.Busy;
        var photo = AppState.Session.Document is { Format: not "pdf" };
        var pageLabel = translatingPage ? "Перекладаємо…" : needsPackages ? "Мовні пакети"
            : photo ? "Перекласти фото" : "Перекласти сторінку";
        var pageGlyph = needsPackages ? "\uf019" : "\uf15c";
        LabelTranslationIcon(TranslatePdfPage, pageGlyph, pageLabel);
        LabelTranslationIcon(DetachedTranslatePdfPage, pageGlyph, pageLabel);
        string? text = null;
        if (address is not null)
        {
            var translated = pdfLayoutRows.Count(HasPdfTranslation);
            if (translatingPage) text = refreshingPdfSentence ? "Перекладаємо речення заново…"
                : photo ? "Перекладаємо фото…" : "Перекладаємо сторінку…";
            else if (!ready && AppState.Session.Document is { } document)
            {
                var routeStatus = readinessPair == (document.Language, address.Target)
                    ? ReadinessMessage() : "Перевіряємо мовні пакети…";
                text = translated > 0 ? $"Збережений переклад · {routeStatus}" : routeStatus;
            }
            else if (pdfLayoutStatus is { Length: > 0 }) text = pdfLayoutStatus;
            if (!translatingPage && keepPdfTranslationOnNavigation && translatingPdfLayout is { } running)
            {
                var progress = refreshingPdfSentence
                    ? $"Оновлюємо переклад речення на сторінці {running.Page + 1}."
                    : $"Перекладаємо сторінку {running.Page + 1}.";
                text = text is null ? progress : $"{text} {progress}";
            }
            if (translated > 0 && translated < pdfLayoutRows.Length)
            {
                var progress = $"Перекладено {translated} з {pdfLayoutRows.Length} фрагментів.";
                text = text is null ? progress : $"{text} {progress}";
            }
            else if (text is null && translated == 0 && pdfLayoutRows.Length > 0)
                text = photo ? "Фото очікує перекладу." : "Сторінка очікує перекладу.";
            if (pdfLayoutRows.Select(row => row.Failure).FirstOrDefault(message => message is not null) is { } failure)
                text = text is null ? failure : $"{text} {failure}";
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
            readinessPair != (document.Language, address.Target) || !pdfLayoutRows.Any(NeedsPreferredPdfTranslation))
            return null;

        var request = new PdfAutoTranslation(address, readinessRequest);
        var limit = pdfLayoutRows.Any(NeedsPdfTranslation) ? 2 : 1;
        return PdfAttemptCount(request) < limit ? request : null;
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
            var label = PdfTranslationTextLabel(row, SourceText.ReadingFontSize - 2, query);
            var action = new Button
            {
                Text = "До оригіналу",
                Style = (Style)Resources["PlayerButton"],
                VerticalOptions = LayoutOptions.Center,
            };
            DescribeButton(action, "Показати відповідний фрагмент на сторінці PDF");
            action.Clicked += (_, _) => result.TrySetResult(row);
            var item = new Grid { ColumnDefinitions = { new(GridLength.Star), new(GridLength.Auto) }, ColumnSpacing = 12 };
            if (!HasPdfTranslation(row) && !string.IsNullOrWhiteSpace(row.SourceText))
            {
                var original = new Label { Text = row.SourceText, FontFamily = "Georgia", FontSize = SourceText.ReadingFontSize - 4 };
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
        DescribeButton(done, "Закрити список фрагментів");
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
        AppState.Session.Document is { } document && SupportsOriginalTranslationLayout(document) &&
        document.Id == address.Document && document.CurrentRevisionId == address.Revision;

    private Label PdfTranslationTextLabel(PdfPageTranslationRow row, double size, string? query = null)
    {
        if (row.Text is not { } text || !HasPdfTranslation(row))
            return new Label { Text = row.Failure ?? UntranslatedPdfFragment, Style = (Style)Resources["ReaderMeta"] };
        var label = new Label { Text = text, FontFamily = "Georgia", FontSize = size, LineHeight = 1.3 };
        if (IsActivePdfRow(row, pdfLayoutSentence)) SetPdfTranslationTextHighlights(label, text, query, null);
        return label;
    }

    private static bool IsActivePdfRow(PdfPageTranslationRow row, Sentence? active) =>
        active is not null && row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16;

    private void SetPdfTranslationTextHighlights(Label label, string text, string? query, string? gloss)
    {
        var matches = GlossRanges(text, gloss)
            .Select(range => (range.Start, range.End, Color: TranslationGlossHighlight)).ToList();
        if (query is not null && DocumentFind.SourceMatch(text, query) is { } match)
        {
            var end = match.StartUtf16 + match.LengthUtf16;
            matches.RemoveAll(range => range.Start < end && range.End > match.StartUtf16);
            matches.Add((match.StartUtf16, end, TranslationSearchMatchColor));
        }
        var offset = 0;
        var formatted = new FormattedString();
        foreach (var (start, end, color) in matches.OrderBy(range => range.Start))
        {
            if (start < offset || end <= start || end > text.Length) continue;
            if (start > offset) formatted.Spans.Add(new Span { Text = text[offset..start] });
            formatted.Spans.Add(new Span { Text = text[start..end], BackgroundColor = color });
            offset = end;
        }
        if (offset == 0)
        {
            if (label.FormattedText is not null) label.FormattedText = null;
            if (label.Text != text) label.Text = text;
            return;
        }
        if (offset < text.Length) formatted.Spans.Add(new Span { Text = text[offset..] });
        label.FormattedText = formatted;
    }

    private async Task LoadPdfPageLayoutAsync(PdfLayoutAddress address, bool cachedOnly,
        PdfLayoutSentenceRefresh? refresh = null, bool userRequested = false)
    {
        if (cachedOnly && translatingPdfLayout == address)
        {
            pendingPdfCacheRefresh = address;
            return;
        }
        var request = ++pdfLayoutRequestId;
        using var cancellation = cachedOnly ? null : new CancellationTokenSource();
        pdfLayoutStatus = cachedOnly ? "Завантажуємо переклад…" : "Перекладаємо сторінку…";
        if (!cachedOnly)
        {
            translatingPdfLayout = address;
            refreshingPdfSentence = refresh is not null;
            keepPdfTranslationOnNavigation = userRequested || refreshingPdfSentence;
            pdfLayoutTranslationCancellation = cancellation;
            if (!refreshingPdfSentence)
            {
                var attempt = new PdfAutoTranslation(address, readinessRequest);
                pdfAutoAttempts = (attempt, PdfAttemptCount(attempt) + 1);
            }
        }
        bool OwnsManualTranslation() => (userRequested || refresh is not null) &&
            cancellation is { IsCancellationRequested: false } &&
            ReferenceEquals(pdfLayoutTranslationCancellation, cancellation);
        RefreshPdfTranslationLayout();
        try
        {
            var source = PdfTranslationSource.FromPage(address.SourceLanguage, address.Revision, address.DetachedText);
            var translation = AppState.Session.PdfPageTranslationAsync(address.Document, address.Page,
                address.Target, cachedOnly, cancellation?.Token ?? default, refresh?.Request, source);
            if (!cachedOnly && refresh is null) _ = FollowPdfTranslationBatchesAsync(address, source, request, translation);
            var rows = await translation;
            var displayMatches = CurrentPdfLayoutAddress() == address;
            var ownsManualTranslation = OwnsManualTranslation();
            if (refresh is { } regenerated)
                NotifyPdfSentenceRefresh(address, regenerated.Source, applyingCurrentRows: ownsManualTranslation && displayMatches);
            if (ownsManualTranslation && !displayMatches)
                RememberPdfPageTranslations(address, rows);
            // Navigation invalidates display reads, but the manual job still owns its source page.
            if ((!ownsManualTranslation && request != pdfLayoutRequestId) || !displayMatches ||
                cancellation?.IsCancellationRequested == true) return;
            if (cachedOnly)
            {
                var failures = pdfLayoutRows.Where(row => row.Failure is not null).ToDictionary(
                    row => (row.StartUtf16, row.EndUtf16), row => row.Failure);
                rows = Array.ConvertAll(rows, row => !HasPdfTranslation(row) &&
                    failures.TryGetValue((row.StartUtf16, row.EndUtf16), out var failure)
                    ? row with { Failure = failure } : row);
            }
            pdfLayoutRows = rows;
            pdfLayoutRowsStale = false;
            pdfLayoutStatus = rows.Length == 0 ? "На сторінці немає тексту для перекладу."
                : refresh is not null ? "Речення перекладено заново."
                : !cachedOnly && rows.All(row => row.Text is null) ? "Переклад сторінки поки недоступний." : "";
        }
        catch (OperationCanceledException) when (cancellation?.IsCancellationRequested == true)
        {
            if (request == pdfLayoutRequestId) pdfLayoutStatus = null;
        }
        catch (Exception error)
        {
            if (request == pdfLayoutRequestId || OwnsManualTranslation() && CurrentPdfLayoutAddress() == address)
                pdfLayoutStatus = error.Message;
        }
        finally
        {
            var finishedActiveJob = !cachedOnly && ReferenceEquals(pdfLayoutTranslationCancellation, cancellation);
            if (finishedActiveJob)
                ReleasePdfLayoutTranslation();
            if (readingPageActive && (finishedActiveJob || request == pdfLayoutRequestId ||
                CurrentPdfLayoutAddress() == address))
                RefreshPdfTranslationLayout();
            if (!cachedOnly && CurrentPdfLayoutAddress() == address &&
                (pendingPdfCacheRefresh == address || request != pdfLayoutRequestId))
                RefreshCachedPdfPageLayout();
        }
    }

    private void NotifyPdfSentenceRefresh(PdfLayoutAddress address, Sentence sentence, bool applyingCurrentRows = false)
    {
        InvalidatePdfTranslationRows(address, sentence, applyingCurrentRows);

        if (address.DetachedText is null)
        {
            InvalidateSentenceTranslation(new TranslationScope(address.Document, address.SourceLanguage, address.Target), sentence.Id);
            if (readingPageActive) _ = RestoreCachedTranslationAsync();
        }
        else if (readingPageActive && TargetCode == address.Target && detachedAddress ==
            new DetachedAddress(address.Document, address.Page, address.DetachedText, address.SourceLanguage))
        {
            RecheckDetachedTranslationCache();
        }
    }

    private void InvalidateReviewedPdfRows(PdfLayoutAddress address, Sentence sentence)
    {
        // A page request can finish with translations read before the review was saved.
        if (translatingPdfLayout is { } running && (running == address || running.SharesCanonicalSource(address)))
            pdfLayoutTranslationCancellation?.Cancel();
        InvalidatePdfTranslationRows(address, sentence);
    }

    private void InvalidatePdfTranslationRows(PdfLayoutAddress address, Sentence sentence, bool applyingCurrentRows = false)
    {
        recentPdfTranslations.RemoveAll(entry => entry.Address.DependsOnSentenceRefresh(address, sentence, entry.Rows));
        if (applyingCurrentRows || pdfLayoutRequest is not { } displayed ||
            !displayed.DependsOnSentenceRefresh(address, sentence, pdfLayoutRows)) return;

        pdfLayoutRowsStale = true;
        // Ignore cache reads started before the new translation was committed.
        pdfLayoutRequestId++;
        RefreshCachedPdfPageLayout();
    }

    private void RememberPdfPageTranslations(PdfLayoutAddress address, PdfPageTranslationRow[] rows)
    {
        recentPdfTranslations.RemoveAll(entry => entry.Address == address);
        if (!rows.Any(HasPdfTranslation)) return;
        recentPdfTranslations.Insert(0, (address, rows));
        if (recentPdfTranslations.Count > RecentPdfTranslationLimit)
            recentPdfTranslations.RemoveAt(RecentPdfTranslationLimit);
    }

    private CancellationTokenSource? ReleasePdfLayoutTranslation()
    {
        var cancellation = pdfLayoutTranslationCancellation;
        translatingPdfLayout = null;
        keepPdfTranslationOnNavigation = false;
        refreshingPdfSentence = false;
        pdfLayoutTranslationCancellation = null;
        return cancellation;
    }

    private async Task FollowPdfTranslationBatchesAsync(PdfLayoutAddress address, PdfTranslationSource source,
        int request, Task translation)
    {
        bool Current() => !translation.IsCompleted && readingPageActive && pdfTranslationLayout &&
            request == pdfLayoutRequestId && CurrentPdfLayoutAddress() == address &&
            (pdfLayoutRows.Length == 0 || pdfLayoutRows.Any(NeedsPreferredPdfTranslation));
        while (Current())
        {
            await Task.WhenAny(translation, Task.Delay(1200));
            if (!Current()) return;
            try
            {
                var rows = await AppState.Session.PdfPageTranslationAsync(address.Document, address.Page,
                    address.Target, cachedOnly: true, source: source);
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
        if (overflowContent is { Active: { } active } overflow && active == pdfLayoutSentence)
        {
            var index = Array.FindIndex(overflow.Rows, row =>
                row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16);
            if (index >= 0)
            {
                FollowOverflowSelection(overflow.Panel, active, overflow.Labels[index]);
                return;
            }
        }
        if (ActiveInlinePdf().Viewport.IsVisible) _ = FollowPdfTranslationScrollAsync(fromOriginal: true);
    }

    private void BackgroundPageTranslated(BackgroundTranslatedPage translated)
    {
        var session = AppState.Session;
        if (!readingPageActive || session.Document is not { } document ||
            translated.DocumentId != document.Id || translated.RevisionId != document.CurrentRevisionId ||
            translated.Source != document.Language || translated.Target != TargetCode)
            return;

        if (pdfTranslationLayout && CurrentPdfLayoutAddress() is { } address && address.Page == translated.Page)
            RefreshCachedPdfPageLayout();
        if (DetachedPdfReader.IsVisible)
        {
            if (detachedAddress is { } detached && detached.Document == document.Id &&
                detached.Page == translated.Page && detached.Language == document.Language)
                RecheckDetachedTranslationCache();
            return;
        }
        var activePage = CurrentNativePdfWindow(document.Id)?.Center ?? session.OriginalPageIndex();
        if (translated.Page != activePage) return;
        RefreshCanonicalTranslationCache();
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
        var (viewport, surface) = ActivePdfTranslationLayout();
        if (!ReferenceEquals(sender, viewport) || !PdfFollowsScroll || pdfTranslationScroll is not null ||
            CurrentPdfLayoutAddress() is not { } address || address != pdfLayoutRequest) return;

        var request = ++pdfTranslationViewportRequest;
        if (readingLayout == ReaderLayout.Translation)
        {
            await SyncPdfTranslationPositionAsync(request, address, viewport, surface);
            return;
        }

        var source = CurrentSourceViewport();
        var overflow = OverflowRowAtScrollPosition(viewport);
        if (overflow is not null) sourceViewportMovement = null;
        await FollowPdfTranslationScrollAsync(fromOriginal: false);
        if (source is null || request != pdfTranslationViewportRequest || CurrentSourceViewport() != source) return;

        if (overflow is null)
        {
            ScheduleSourceViewportSync(source.View);
            return;
        }

        await Task.Delay(220);
        if (request != pdfTranslationViewportRequest || pdfTranslationScroll is not null ||
            CurrentPdfLayoutAddress() != address || CurrentSourceViewport() != source ||
            OverflowRowAtScrollPosition(viewport) is not { } row ||
            surface.Drawable is not TranslatedPdfDrawing) return;
        if (pdfLayoutSentence is not { } active || active.StartUtf16 != row.StartUtf16 || active.EndUtf16 != row.EndUtf16)
            await SelectTranslatedPdfRowAsync(row);
    }

    private async Task SyncPdfTranslationPositionAsync(int request, PdfLayoutAddress address,
        ScrollView viewport, GraphicsView surface)
    {
        if (CurrentSourceViewport(requireVisible: false) is not { } scope) return;
        await Task.Delay(220);
        if (request != pdfTranslationViewportRequest || readingLayout != ReaderLayout.Translation ||
            pdfTranslationScroll is not null || !viewport.IsVisible || CurrentPdfLayoutAddress() != address ||
            scope != CurrentSourceViewport(requireVisible: false) ||
            surface.Drawable is not TranslatedPdfDrawing drawing) return;

        var active = pdfLayoutSentence;
        var row = OverflowRowAtScrollPosition(viewport) ??
            drawing.FirstVisibleRow(viewport.ScrollY, viewport.ScrollY + viewport.Height);
        if (row is not null && !IsActivePdfRow(row, active))
            await SelectTranslatedPdfRowAsync(row);
    }

    private async void TranslatedPdfTapped(object? sender, TappedEventArgs args)
    {
        var (viewport, surface) = ActivePdfTranslationLayout();
        if (!ReferenceEquals(sender, surface) || !viewport.IsVisible ||
            args.GetPosition(surface) is not { } point || surface.Drawable is not TranslatedPdfDrawing drawing ||
            drawing.HitTest(point) is not { } fragment) return;
        if (!await SelectTranslatedPdfRowAsync(fragment)) return;
        if (overflowContent is not { } overflow) return;
        var index = Array.FindIndex(overflow.Rows, row => SamePdfSourceFragment(row, fragment));
        if (index >= 0) FollowOverflowSelection(overflow.Panel, pdfLayoutSentence, overflow.Labels[index]);
    }

    private static bool SamePdfSourceFragment(PdfPageTranslationRow first, PdfPageTranslationRow second) =>
        first.SourceSentenceId == second.SourceSentenceId &&
        first.StartUtf16 == second.StartUtf16 && first.EndUtf16 == second.EndUtf16;

    private async Task<bool> SelectTranslatedPdfRowAsync(PdfPageTranslationRow fragment)
    {
        var session = AppState.Session;
        if (!readingPageActive || CurrentPdfLayoutAddress() is not { } address ||
            address != pdfLayoutRequest || !pdfLayoutRows.Any(row => SamePdfSourceFragment(row, fragment))) return false;

        var request = ++pdfTranslationSelectionRequest;
        bool Current() => readingPageActive && AppState.Session == session &&
            request == pdfTranslationSelectionRequest && pdfLayoutRequest == address &&
            CurrentPdfLayoutAddress() == address &&
            pdfLayoutRows.Any(row => SamePdfSourceFragment(row, fragment));

        while (session.Busy && Current()) await session.WaitUntilIdleAsync();
        if (!Current()) return false;
        await SelectPdfSourceRangeAsync(address.Source, fragment.StartUtf16, fragment.EndUtf16, fragment.SourceSentenceId, address);
        return Current() && pdfFragmentFocus ==
            (address, fragment.SourceSentenceId, fragment.StartUtf16, fragment.EndUtf16);
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

    private void ClearTranslationFocus()
    {
        var foundTranslation = documentFindFocus?.Translation is not null;
        if (pdfFragmentFocus is null && !foundTranslation) return;
        pdfFragmentFocus = null;
        if (!foundTranslation)
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
        if (readingLayout == ReaderLayout.Translation || !PdfFollowsScroll ||
            pdfTranslationScroll is not null || !original.IsVisible || !translated.IsVisible ||
            originalSurface.Height <= 0 || translatedSurface.Height <= 0) return;

        var drawing = translatedSurface.Drawable as TranslatedPdfDrawing;
        if (fromOriginal && drawing is not null &&
            FindActiveOverflowLabel(original, originalSurface.Height, translated, drawing) is { } overflowTarget)
        {
            var (label, top) = overflowTarget;
            if (label.Height > 0 && !OverflowLabelVisible(translated.ScrollY, translated.Height, top, label.Height))
                await ScrollPdfTranslationAsync(() => ScrollToOverflowLabelAsync(translated, label));
            return;
        }

        var source = fromOriginal ? original : translated;
        var destination = fromOriginal ? translated : original;
        var sourceHeight = fromOriginal ? originalSurface.Height : translatedSurface.Height;
        var destinationHeight = fromOriginal ? translatedSurface.Height : originalSurface.Height;
        var sourcePosition = source.ScrollY * destinationHeight / sourceHeight;
        if (!fromOriginal && OverflowRowAtScrollPosition(translated) is { } row)
        {
            if (drawing?.SourceBounds(row) is not { } bounds) return;
            sourcePosition = bounds.Y * destinationHeight - Math.Min(44, destination.Height * 0.12);
        }
        var target = Math.Clamp(sourcePosition, 0,
            Math.Max(0, destinationHeight - destination.Height));
        if (Math.Abs(destination.ScrollY - target) < 1) return;

        await ScrollPdfTranslationAsync(() => destination.ScrollToAsync(0, target, animated: false));
    }

    private (Label Label, double Top)? FindActiveOverflowLabel(ScrollView original, double paperHeight,
        ScrollView translated, TranslatedPdfDrawing drawing)
    {
        if (pdfLayoutSentence is not { } active || original.Height <= 0 ||
            overflowContent is not { } overflow || !overflow.Panel.IsVisible ||
            !ReferenceEquals(overflow.Panel.Parent, translated.Content)) return null;

        var top = original.ScrollY / paperHeight;
        var bottom = (original.ScrollY + original.Height) / paperHeight;
        for (var index = 0; index < overflow.Rows.Length; index++)
        {
            var row = overflow.Rows[index];
            if (!IsActivePdfRow(row, active) || drawing.SourceBounds(row) is not { } bounds ||
                bounds.Y >= bottom || bounds.Y + bounds.Height <= top) continue;
            var label = overflow.Labels[index];
            return (label, overflow.Panel.Y + label.Y);
        }
        return null;
    }

    private static bool OverflowLabelVisible(double viewportTop, double viewportHeight, double labelTop, double labelHeight)
    {
        var overlap = Math.Min(viewportTop + viewportHeight, labelTop + labelHeight) - Math.Max(viewportTop, labelTop);
        return overlap >= Math.Min(viewportHeight, labelHeight) - 1;
    }

    private static Task ScrollToOverflowLabelAsync(ScrollView viewport, Label label) =>
        viewport.ScrollToAsync(label,
            label.Height > viewport.Height ? ScrollToPosition.Start : ScrollToPosition.MakeVisible, animated: false);

    private async Task ScrollPdfTranslationAsync(Func<Task> scroll)
    {
        var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        pdfTranslationScroll = completion;
        try { await scroll(); }
        catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
        finally
        {
            pdfTranslationScroll = null;
            completion.TrySetResult();
        }
    }

    private async Task WaitForPdfTranslationScrollAsync()
    {
        while (pdfTranslationScroll is { } scroll) await scroll.Task;
    }

    private PdfPageTranslationRow? OverflowRowAtScrollPosition(ScrollView viewport)
    {
        if (overflowContent is not { } overflow || overflow.Labels.Length == 0 || !overflow.Panel.IsVisible ||
            !ReferenceEquals(overflow.Panel.Parent, viewport.Content))
            return null;

        // Track the reading position inside the viewport, even while the paper's
        // bottom edge and the first overflow entries are both visible.
        var offset = viewport.ScrollY + viewport.Height * 0.25 - overflow.Panel.Y;
        if (offset < 0) return null;
        var index = Array.FindIndex(overflow.Labels, label => label.Height > 0 && label.Y + label.Height > offset);
        return index >= 0 ? overflow.Rows[index] : null;
    }

    private void ClearPdfTranslationBackground()
    {
        PdfTranslationBackground.IsVisible = DetachedPdfTranslationBackground.IsVisible = false;
        PdfTranslationBackground.Source = DetachedPdfTranslationBackground.Source = null;
        pdfTranslationBackgroundPreview = null;
    }

    private void DrawPdfTranslationLayout()
    {
        var (viewport, surface) = ActivePdfTranslationLayout();
        if (!viewport.IsVisible) return;
        var detached = DetachedPdfReader.IsVisible;
        var background = detached ? DetachedPdfTranslationBackground : PdfTranslationBackground;
        var source = pdfSourceLayout is { } layout && layout.Matches(pdfLayoutRequest) ? layout : null;
        if (TranslationVisible && pdfLayoutRequest?.Source == inlinePdfAddress && pdfLayoutRows.Length > 0 &&
            inlinePdfPreview is { } preview)
        {
            if (background.Source is null || !ReferenceEquals(pdfTranslationBackgroundPreview, preview))
            {
                ClearPdfTranslationBackground();
                background.Source = InlinePreviewSource(preview);
                pdfTranslationBackgroundPreview = preview;
            }
            background.IsVisible = true;
        }
        else ClearPdfTranslationBackground();
        var gloss = (detached ? DetachedWordCard : WordCard).IsVisible ? PopupGloss(detached) : null;
        var activeSentence = pdfLayoutSentence;
        QueueGlossAlignment(gloss, pdfLayoutRows.Where(row => IsActivePdfRow(row, activeSentence)).Select(row => row.Text));
        var glossResults = gloss is null ? 0 : glossAlignment?.Resolved.Count ?? 0;
        var query = PdfTranslationFindQuery();
        if (pdfDrawingPlan is not { } cached || cached.Rows != pdfLayoutRows || !SamePdfGeometry(cached.Source, source) ||
            cached.PreserveArtwork != background.IsVisible)
        {
            cached = (pdfLayoutRows, source, background.IsVisible,
                PlanPdfDrawing(pdfLayoutRows, source, background.IsVisible));
            pdfDrawingPlan = cached;
        }
        var drawing = new TranslatedPdfDrawing(pdfLayoutRows,
            activeSentence, text => GlossRanges(text, gloss), background.IsVisible, query,
            PdfReadingHorizontalBounds(), cached.Plan, surface);
        var overflow = detached ? DetachedPdfTranslationOverflow : PdfTranslationOverflow;
        drawing.OverflowChanged = rows => Dispatcher.Dispatch(() =>
        {
            if (viewport.IsVisible && ReferenceEquals(surface.Drawable, drawing))
            {
                var description = string.Join(" ", pdfLayoutRows.Except(rows)
                    .Select(row => HasPdfTranslation(row) ? row.Text : row.Failure ?? UntranslatedPdfFragment));
                if (SemanticProperties.GetDescription(surface) != description)
                    SemanticProperties.SetDescription(surface, description);
                var bounds = drawing.MatchBounds ?? drawing.ActiveBounds;
                ShowPdfTranslationOverflow(overflow, rows, bounds is not null, query, gloss, glossResults);
                if (bounds is { } activeBounds && pdfLayoutSentence is { } active &&
                    !rows.Any(row => row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16))
                    _ = FollowRenderedPdfSentenceAsync(viewport, surface, drawing, active, activeBounds);
                else
                    lastPdfTranslationFollow = null;
            }
        });
        surface.Drawable = drawing;
        surface.Invalidate();
    }

    private async Task FollowRenderedPdfSentenceAsync(ScrollView viewport, GraphicsView surface,
        TranslatedPdfDrawing drawing, Sentence active, RectF bounds)
    {
        await WaitForPdfTranslationScrollAsync();
        if (!PdfFollowsScroll || !viewport.IsVisible || viewport.Height <= 0 ||
            !ReferenceEquals(surface.Drawable, drawing) || pdfLayoutSentence != active ||
            pdfLayoutRequest is not { } address) return;
        var key = (address, active.Id, bounds, viewport.Height);
        if (lastPdfTranslationFollow == key) return;
        lastPdfTranslationFollow = key;

        var margin = Math.Min(44, viewport.Height * 0.12);
        if (bounds.Top >= viewport.ScrollY + margin &&
            bounds.Bottom <= viewport.ScrollY + viewport.Height - margin) return;
        var target = Math.Clamp(bounds.Top - margin, 0, Math.Max(0, surface.Height - viewport.Height));
        if (Math.Abs(viewport.ScrollY - target) < 1) return;
        await ScrollPdfTranslationAsync(() => viewport.ScrollToAsync(0, target, animated: false));
    }

    private void ShowPdfTranslationOverflow(VerticalStackLayout panel, PdfPageTranslationRow[] rows, bool hasRenderedActive,
        string? query, string? gloss, int glossResults)
    {
        var active = pdfLayoutSentence;
        var previous = overflowContent;
        Label[] labels;
        if (previous is { } current && current.Panel == panel && current.Rows.SequenceEqual(rows))
        {
            if (current.Active == active && current.Query == query && current.Gloss == gloss &&
                current.GlossResults == glossResults) return;
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
                    var label = PdfTranslationTextLabel(row, SourceText.ReadingFontSize - 3);
                    var tap = new TapGestureRecognizer();
                    tap.Tapped += async (_, _) => await SelectTranslatedPdfRowAsync(row);
                    label.GestureRecognizers.Add(tap);
                    panel.Children.Add(label);
                    labels[index] = label;
                }
            }
        }
        overflowContent = (panel, rows, labels, active, query, gloss, glossResults);

        Label? activeLabel = null;
        for (var index = 0; index < labels.Length; index++)
        {
            var row = rows[index];
            var selected = IsActivePdfRow(row, active);
            if (row.Text is { } text && HasPdfTranslation(row) && (selected || labels[index].FormattedText is not null))
                SetPdfTranslationTextHighlights(labels[index], text, selected ? query : null, selected ? gloss : null);
            labels[index].BackgroundColor = selected ? Color.FromArgb("#FFF1CF") : Colors.Transparent;
            if (selected) activeLabel ??= labels[index];
        }
        var previouslyInOverflow = previous?.Rows.Any(row => IsActivePdfRow(row, active)) == true;
        if ((activeLabel is not null || !hasRenderedActive) &&
            (previous?.Active != active || previous?.Panel != panel || previous?.Query != query ||
                (activeLabel is not null && previous?.Labels != labels) ||
                previouslyInOverflow != (activeLabel is not null)))
            FollowOverflowSelection(panel, active, activeLabel);
    }

    private void FollowOverflowSelection(VerticalStackLayout panel, Sentence? active, Label? label)
    {
        if (active is null || pdfLayoutRequest is not { } address) return;
        if (label is { Height: <= 0 })
        {
            EventHandler? measured = null;
            measured = (_, _) =>
            {
                if (label.Height <= 0) return;
                label.SizeChanged -= measured;
                if (pdfLayoutRequest == address) FollowOverflowSelection(panel, active, label);
            };
            label.SizeChanged += measured;
            return;
        }

        Dispatcher.Dispatch(async () =>
        {
            await WaitForPdfTranslationScrollAsync();
            var (viewport, surface) = ActivePdfTranslationLayout();
            if (!viewport.IsVisible || pdfLayoutRequest != address || pdfLayoutSentence != active ||
                overflowContent is not { } current || current.Panel != panel || current.Active != active ||
                (label is not null && !current.Labels.Contains(label))) return;
            if (label is null)
            {
                if (viewport.ScrollY <= Math.Max(0, surface.Height - viewport.Height) + 1) return;
                if (PdfFollowsScroll)
                {
                    await FollowPdfTranslationScrollAsync(fromOriginal: true);
                    return;
                }
            }

            await ScrollPdfTranslationAsync(() => label is null ? viewport.ScrollToAsync(0, 0, animated: false)
                : ScrollToOverflowLabelAsync(viewport, label));
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
                style.Italic, style.Ink is { } ink ? Rgba(ink) : null, Rgba(style.Background), style.MaskSafe);
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

        internal PdfSampleBox? SourceBounds(PdfPageTranslationRow row)
        {
            var index = Array.IndexOf(rows, row);
            return index >= 0 ? layout.Anchors[index].Bounds : null;
        }

        internal PdfPageTranslationRow? HitTest(Point point)
        {
            foreach (var (bounds, source) in hitTargets)
                if (point.X >= bounds.Left && point.X <= bounds.Right &&
                    point.Y >= bounds.Top && point.Y <= bounds.Bottom)
                    return source;
            return null;
        }

        internal PdfPageTranslationRow? FirstVisibleRow(double top, double bottom)
        {
            PdfPageTranslationRow? first = null;
            var firstTop = float.PositiveInfinity;
            foreach (var (bounds, row) in hitTargets)
            {
                if (bounds.Top >= bottom || bounds.Bottom <= top) continue;
                if (IsActivePdfRow(row, active)) return row;
                if (bounds.Top >= firstTop) continue;
                first = row;
                firstTop = bounds.Top;
            }
            return first;
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
                !preserveArtwork)
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
                DrawRegion(region);
            foreach (var index in layout.Pending)
                DrawPendingFragment(canvas, page, rows[index], layout.Anchors[index]);
            foreach (var paint in foreground) paint();

            OverflowChanged?.Invoke(overflow.OrderBy(row => row.StartUtf16).ToArray());

            void DrawRegion(PdfTranslationRegion region)
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
                var area = Scale(region.Bounds, page);
                var sideways = region.Rotation is 90 or 270;
                var textArea = sideways
                    ? new RectF(area.X + (area.Width - area.Height) / 2, area.Y + (area.Height - area.Width) / 2,
                        area.Height, area.Width)
                    : area;
                var preferredSize = (float)((sideways ? page.Width : page.Height) * region.PreferredFontHeight);
                if (region.LimitFontByWidth) preferredSize = Math.Min((sideways ? page.Height : page.Width) * 0.045f, preferredSize);
                var minimumSize = Math.Max(2, preferredSize * 0.60f);
                var translated = text.ToString();
                var font = new Microsoft.Maui.Graphics.Font(appearance?.FontFamily ?? "Georgia", appearance?.Weight ?? 400,
                    appearance?.Italic == true ? FontStyleType.Italic : FontStyleType.Normal);
                var fit = textArea.Width >= 2 && textArea.Height >= 2
                    ? FitText(canvas, translated, font, textArea.Width, textArea.Height, preferredSize, minimumSize) : null;
                if (fit is null && region.FallbackRegions is { Length: > 0 } fallbackRegions)
                {
                    var fallbackIndices = fallbackRegions.SelectMany(fallback => fallback.Fragments).ToHashSet();
                    overflow.AddRange(region.Fragments.Where(index => !fallbackIndices.Contains(index)).Select(index => rows[index]));
                    foreach (var fallback in fallbackRegions) DrawRegion(fallback);
                    return;
                }
                foreach (var mask in region.Masks)
                {
                    canvas.FillColor = ColorFrom(mask.Background);
                    canvas.FillRectangle(Scale(mask.Bounds, page));
                }
                if (fit is null)
                {
                    overflow.AddRange(fragments.Select(fragment => fragment.Source));
                    var selected = fragments.Find(fragment => fragment.Active);
                    var linked = selected.Active ? selected : fragments[0];
                    foreground.Add(() =>
                    {
                        var color = Color.FromArgb(linked.Active ? "#88432C" : "#58584E");
                        if (DrawFragmentBadge(canvas, area, "Переклад нижче", "↓", color) is { } badge)
                            hitTargets.Add((badge, linked.Source));
                    });
                    return;
                }
                var ink = appearance?.Ink ?? Color.FromArgb("#302D27");
                foreground.Add(() => DrawText(canvas, translated, textArea, fragments, font, ink, fit, region.Alignment,
                    region.Rotation));
            }
        }

        private void DrawPendingFragment(ICanvas canvas, RectF page, PdfPageTranslationRow row, PdfTranslationAnchor anchor)
        {
            if (anchor.Bounds is not { } sourceBounds) return;
            var bounds = Scale(sourceBounds, page);
            hitTargets.Add((bounds, row));
            foreach (var mask in anchor.PendingMasks ?? [])
            {
                canvas.FillColor = ColorFrom(mask.Background);
                canvas.FillRectangle(Scale(mask.Bounds, page));
            }

            if (active is not null && row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16)
            {
                ActiveBounds = ActiveBounds is { } previous ? Union(previous, bounds) : bounds;
                canvas.FillColor = Color.FromRgba(255, 212, 122, 64);
                canvas.FillRectangle(bounds);
            }

            var spot = anchor.PendingMasks?.FirstOrDefault()?.Bounds is { } firstLine ? Scale(firstLine, page) : bounds;
            DrawFragmentBadge(canvas, spot, PdfFragmentState(row), row.Failure is null ? "…" : "!",
                Color.FromArgb(row.Failure is null ? "#58584E" : "#88432C"));
        }

        private static RectF? DrawFragmentBadge(ICanvas canvas, RectF spot, string label, string compact, Color color)
        {
            var height = Math.Min(21, spot.Height);
            if (height <= 0 || spot.Width <= 0) return null;
            var font = Microsoft.Maui.Graphics.Font.Default;
            var size = height * 0.65f;
            var width = canvas.GetStringSize(label, font, size).Width + height;
            if (width > spot.Width)
            {
                label = compact;
                width = Math.Min(spot.Width, height);
                size = Math.Min(size, width * 0.65f);
            }
            canvas.FillColor = color;
            canvas.FillRoundedRectangle(spot.Left, spot.Top, width, height, Math.Min(width, height) * 0.25f);
            canvas.Font = font;
            canvas.FontSize = size;
            canvas.FontColor = Colors.White;
            canvas.DrawString(label, spot.Left, spot.Top, width, height,
                HorizontalAlignment.Center, VerticalAlignment.Center, TextFlow.ClipBounds);
            return new RectF(spot.Left, spot.Top, width, height);
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
                var text = HasPdfTranslation(row) ? row.Text! : PdfFragmentState(row);
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
            Microsoft.Maui.Graphics.Font font, Color ink, PdfTextFit layout, string alignment = "left", int rotation = 0)
        {
            var lightInk = ink.Red * 0.21f + ink.Green * 0.72f + ink.Blue * 0.07f > 0.55f;
            var size = layout.Size;
            var lineHeight = layout.LineHeight;
            canvas.Font = font;
            canvas.FontColor = ink;
            canvas.FontSize = size;
            canvas.SaveState();
            var centerX = area.X + area.Width / 2;
            var centerY = area.Y + area.Height / 2;
            if (rotation != 0) canvas.Rotate(rotation, centerX, centerY);
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
                    var sourceBounds = RotateBounds(bounds, centerX, centerY, rotation);
                    hitTargets.Add((sourceBounds, fragment.Source));
                    if (!fragment.Active) continue;
                    ActiveBounds = ActiveBounds is { } previous ? Union(previous, sourceBounds) : sourceBounds;
                    canvas.FillColor = Color.FromArgb(lightInk ? "#4A391E" : "#FFF1CF");
                    canvas.FillRectangle(bounds);
                    if (fragment.Match is { } match)
                    {
                        var matchStart = Math.Max(match.StartUtf16 - range.Start, start);
                        var matchEnd = Math.Min(match.StartUtf16 + match.LengthUtf16 - range.Start, end);
                        if (matchStart < matchEnd)
                        {
                            var matchBounds = BoundsFor(matchStart, matchEnd);
                            var sourceMatch = RotateBounds(matchBounds, centerX, centerY, rotation);
                            MatchBounds = MatchBounds is { } previousMatch ? Union(previousMatch, sourceMatch) : sourceMatch;
                            canvas.FillColor = lightInk ? Color.FromArgb("#604321") : TranslationSearchMatchColor;
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
                            canvas.FillColor = lightInk ? Color.FromArgb("#604321") : TranslationGlossHighlight;
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

        private static RectF RotateBounds(RectF bounds, float centerX, float centerY, int rotation) => rotation switch
        {
            90 => new(centerX + centerY - bounds.Bottom, centerY - centerX + bounds.Left, bounds.Height, bounds.Width),
            180 => new(2 * centerX - bounds.Right, 2 * centerY - bounds.Bottom, bounds.Width, bounds.Height),
            270 => new(centerX - centerY + bounds.Top, centerY + centerX - bounds.Right, bounds.Height, bounds.Width),
            _ => bounds,
        };
    }
}
