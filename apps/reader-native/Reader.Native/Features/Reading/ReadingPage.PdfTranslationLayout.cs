using Reader.Contracts;
using Reader.Native.Services;
using System.Text;
using System.Text.RegularExpressions;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record PdfLayoutAddress(string Document, int Page, string? Revision, string Target, string? DetachedText);
    private sealed record PdfAutoTranslation(PdfLayoutAddress Address, int RouteRequest);
    private bool pdfTranslationLayout = true;
    private bool syncingPdfTranslationScroll;
    private PdfLayoutAddress? pdfLayoutRequest;
    private PdfLayoutAddress? translatingPdfLayout;
    private PdfPageTranslationRow[] pdfLayoutRows = [];
    private OcrLine[] pdfLayoutGeometry = [];
    private int pdfLayoutOffset;
    private Sentence? pdfLayoutSentence;
    private string? pdfLayoutStatus;
    private int pdfLayoutRequestId;
    private bool refreshingPdfLayout;
    private PdfAutoTranslation? pendingPdfAutoTranslation;
    private PdfAutoTranslation? lastPdfAutoTranslation;
    private (VerticalStackLayout Panel, PdfPageTranslationRow[] Rows, Label[] Labels, Sentence? Active)? overflowContent;

    private (ScrollView Viewport, GraphicsView Surface) ActivePdfTranslationLayout() =>
        DetachedPdfReader.IsVisible
            ? (DetachedPdfTranslationViewport, DetachedPdfTranslationSurface)
            : (PdfTranslationViewport, PdfTranslationSurface);

    private bool PdfUsesPageTranslation => inlinePdf && pdfTranslationLayout && TranslationVisible;

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
        if (pdfLayoutRequest is { } address && translatingPdfLayout is null)
            await LoadPdfPageLayoutAsync(address, cachedOnly: false);
    }

    private PdfLayoutAddress? CurrentPdfLayoutAddress() =>
        inlinePdf && inlinePdfAddress is { } page && TargetCode is { } target &&
        AppState.Session.Document is { } document && page.Document == document.Id &&
        target != document.Language && (!PdfPageNotice.IsVisible || DetachedPdfReader.IsVisible) &&
        (CurrentNativePdfWindow(document.Id) is not { } window || window.Center == page.Page)
            ? new(page.Document, page.Page, document.CurrentRevisionId, target, page.DetachedText)
            : null;

    private void RefreshPdfTranslationLayout()
    {
        var address = CurrentPdfLayoutAddress();
        AppState.TranslationWorker?.SetVisiblePdf(readingPageActive && TranslationVisible &&
            !(autoTranslate && PdfUsesPageTranslation) &&
            address is { Revision: { } revision }
                ? new BackgroundTranslatedPage(address.Document, revision, address.Page, address.Target) : null,
            address?.DetachedText);
        var available = address is not null;
        var detached = DetachedPdfReader.IsVisible;
        var shown = available && pdfTranslationLayout;
        PdfTranslationMode.IsVisible = available && !detached;
        DetachedPdfTranslationMode.IsVisible = available && detached;
        PdfTranslationMode.Text = DetachedPdfTranslationMode.Text = shown ? "Список" : "Верстка";
        PdfTranslationViewport.IsVisible = TranslatePdfPage.IsVisible = shown && !detached;
        DetachedPdfTranslationViewport.IsVisible = DetachedTranslatePdfPage.IsVisible = shown && detached;
        TranslationViewport.IsVisible = !PdfTranslationViewport.IsVisible;
        DetachedTranslationViewport.IsVisible = !DetachedPdfTranslationViewport.IsVisible;
        var translatingPage = translatingPdfLayout == address && address is not null;
        TranslatePdfPage.IsEnabled = DetachedTranslatePdfPage.IsEnabled = available && translatingPdfLayout is null;
        TranslatePdfPage.Text = DetachedTranslatePdfPage.Text = translatingPage ? "Перекладаємо…" : "Перекласти сторінку";
        if (address != pdfLayoutRequest)
        {
            pdfLayoutRequest = address;
            pdfLayoutRequestId++;
            lastPdfAutoTranslation = null;
            pdfLayoutRows = [];
            pdfLayoutGeometry = [];
            pdfLayoutSentence = null;
            pdfLayoutStatus = null;
            overflowContent = null;
            PdfTranslationOverflow.IsVisible = DetachedPdfTranslationOverflow.IsVisible = false;
            if (shown && address is not null) _ = LoadPdfPageLayoutAsync(address, cachedOnly: true);
        }
        else if (shown && address is not null && pdfLayoutRows.Length == 0 && pdfLayoutStatus is null)
            _ = LoadPdfPageLayoutAsync(address, cachedOnly: true);
        var hasRows = pdfLayoutRows.Any(row => !string.IsNullOrWhiteSpace(row.Text));
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
        return request == lastPdfAutoTranslation ? null : request;
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
        await Task.Delay(350);
        if (!ReferenceEquals(pendingPdfAutoTranslation, request)) return;
        pendingPdfAutoTranslation = null;
        if (NextPdfAutoTranslation() == request)
            await LoadPdfPageLayoutAsync(request.Address, cachedOnly: false);
    }

    private async void ReadPdfTranslationTextClicked(object? sender, EventArgs args)
    {
        var rows = pdfLayoutRows.Where(row => !string.IsNullOrWhiteSpace(row.Text)).ToArray();
        if (rows.Length == 0 || pdfLayoutRequest is not { } address || CurrentPdfLayoutAddress() != address) return;
        var text = new VerticalStackLayout { Padding = new Thickness(22, 10), Spacing = 9 };
        var viewport = new ScrollView { Content = text };
        Label? activeLabel = null;
        foreach (var row in rows)
        {
            var label = new Label { Text = row.Text, FontFamily = "Georgia", FontSize = 20, LineHeight = 1.3 };
            if (pdfLayoutSentence is { } sentence &&
                row.StartUtf16 < sentence.EndUtf16 && row.EndUtf16 > sentence.StartUtf16)
            {
                label.BackgroundColor = Color.FromArgb("#FFF1CF");
                activeLabel ??= label;
            }
            text.Children.Add(label);
        }
        var done = new Button { Text = "Закрити", HorizontalOptions = LayoutOptions.End, Margin = new Thickness(12) };
        var layout = new Grid { RowDefinitions = { new(GridLength.Auto), new(GridLength.Star) } };
        layout.Add(done);
        layout.Add(viewport, 0, 1);
        var page = new ContentPage { Title = $"Переклад сторінки {address.Page + 1}", Content = layout };
        done.Clicked += async (_, _) => await Navigation.PopModalAsync();
        await Navigation.PushModalAsync(page);
        if (activeLabel is not null) await viewport.ScrollToAsync(activeLabel, ScrollToPosition.MakeVisible, animated: false);
    }

    private async Task LoadPdfPageLayoutAsync(PdfLayoutAddress address, bool cachedOnly)
    {
        var request = ++pdfLayoutRequestId;
        pdfLayoutStatus = cachedOnly ? "Завантажуємо переклад…" : "Перекладаємо сторінку…";
        if (!cachedOnly)
        {
            translatingPdfLayout = address;
            lastPdfAutoTranslation = new(address, readinessRequest);
        }
        RefreshPdfTranslationLayout();
        try
        {
            var rows = await AppState.Session.PdfPageTranslationAsync(address.Document, address.Page,
                address.Target, cachedOnly);
            if (request != pdfLayoutRequestId || CurrentPdfLayoutAddress() != address) return;
            pdfLayoutRows = rows;
            pdfLayoutStatus = rows.Any(row => row.Text is not null)
                ? "" : cachedOnly ? "Перекладіть сторінку, щоб заповнити її." : "Для цієї сторінки переклад недоступний.";
        }
        catch (Exception error)
        {
            if (request == pdfLayoutRequestId) pdfLayoutStatus = error.Message;
        }
        finally
        {
            if (!cachedOnly && translatingPdfLayout == address) translatingPdfLayout = null;
            RefreshPdfTranslationLayout();
        }
    }

    private void PdfTranslationViewportChanged(object? sender, EventArgs args)
    {
        if (!inlinePdf) return;
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
        if (!readingPageActive || refreshingPdfLayout || !pdfTranslationLayout || translatingPdfLayout is not null ||
            CurrentPdfLayoutAddress() is not { } address) return;
        refreshingPdfLayout = true;
        try
        {
            await Task.Delay(150);
            if (readingPageActive && pdfTranslationLayout && CurrentPdfLayoutAddress() == address && translatingPdfLayout is null)
                await LoadPdfPageLayoutAsync(address, cachedOnly: true);
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
        if (DetachedPdfReader.IsVisible)
        {
            if (detachedAddress is { } detached && !loadingDetachedSentence)
                await LoadDetachedSentenceAsync(detached, position: null, sourceOffset: fragment.StartUtf16);
            return;
        }
        if (session.Document is not { } document || document.Id != address.Document) return;
        var position = Array.FindIndex(document.Sentences,
            sentence => sentence.StartUtf16 < fragment.EndUtf16 && sentence.EndUtf16 > fragment.StartUtf16);
        if (position >= 0 && position != document.Position) await session.SelectSentenceAsync(position);
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
        background.IsVisible = background.Source is not null && pdfLayoutGeometry.Length > 0;
        var gloss = detached
            ? (DetachedWordCard.IsVisible && detachedWordTranslation?.Target == TargetCode ? detachedWordTranslation?.Text : null)
            : (WordCard.IsVisible && wordTranslation?.Target == TargetCode ? wordTranslation?.Text : null);
        var drawing = new TranslatedPdfDrawing(pdfLayoutRows, pdfLayoutGeometry, pdfLayoutOffset,
            pdfLayoutSentence, gloss, pdfLayoutStatus, background.IsVisible);
        var overflow = detached ? DetachedPdfTranslationOverflow : PdfTranslationOverflow;
        drawing.OverflowChanged = rows => Dispatcher.Dispatch(() =>
        {
            if (viewport.IsVisible && ReferenceEquals(surface.Drawable, drawing))
            {
                var description = string.Join(" ", pdfLayoutRows.Except(rows).Select(row => row.Text)
                    .Where(text => !string.IsNullOrWhiteSpace(text)));
                if (SemanticProperties.GetDescription(surface) != description)
                    SemanticProperties.SetDescription(surface, description);
                ShowPdfTranslationOverflow(overflow, rows);
            }
        });
        surface.Drawable = drawing;
        surface.Invalidate();
    }

    private void ShowPdfTranslationOverflow(VerticalStackLayout panel, PdfPageTranslationRow[] rows)
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
                    var label = new Label { Text = row.Text, FontFamily = "Georgia", FontSize = 19 };
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
        if (previous?.Active != active || previous?.Panel != panel || previouslyInOverflow != (activeLabel is not null))
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
        Sentence? active, string? gloss, string? status, bool preserveArtwork) : IDrawable
    {
        private sealed record Region(RectF Bounds, List<PdfPageTranslationRow> Rows);
        private readonly record struct Fragment(PdfPageTranslationRow Source, int Start, int End, bool Active);
        private readonly record struct WrappedLine(int Start, int End);
        private readonly List<(RectF Bounds, PdfPageTranslationRow Source)> hitTargets = [];
        internal Action<PdfPageTranslationRow[]>? OverflowChanged { get; set; }

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
            canvas.FillColor = Colors.White;
            if (page.Width <= 0 || page.Height <= 0) return;
            if (preserveArtwork)
            {
                foreach (var line in geometry.Where(line => line.SourceSpan is not null))
                {
                    var x = page.X + (float)line.X * page.Width;
                    var y = page.Y + (float)line.Y * page.Height;
                    canvas.FillRectangle(x - 1, y - 1, (float)line.Width * page.Width + 2,
                        (float)line.Height * page.Height + 2);
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
                    if (!string.IsNullOrWhiteSpace(row.Text)) overflow.Add(row);
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

            foreach (var region in regions)
            {
                var text = new StringBuilder();
                var fragments = new List<Fragment>();
                var untranslatedActive = false;
                foreach (var row in region.Rows.OrderBy(row => row.StartUtf16))
                {
                    var isActive = active is not null && row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16;
                    if (string.IsNullOrWhiteSpace(row.Text))
                    {
                        untranslatedActive |= isActive;
                        continue;
                    }
                    if (text.Length > 0) text.Append(' ');
                    var start = text.Length;
                    text.Append(row.Text);
                    fragments.Add(new Fragment(row, start, text.Length, isActive));
                }
                if (untranslatedActive && !fragments.Any(fragment => fragment.Active))
                {
                    canvas.FillColor = Color.FromArgb("#FFF1CF");
                    canvas.FillRoundedRectangle(region.Bounds, 3);
                }
                if (text.Length > 0 && !DrawFitted(canvas, text.ToString(), region.Bounds, fragments, gloss))
                    overflow.AddRange(fragments.Select(fragment => fragment.Source));
            }

            var message = !string.IsNullOrWhiteSpace(status) ? status :
                overflow.Count > 0 ? "Частина перекладу під сторінкою ↓" : null;
            if (message is not null)
                DrawFitted(canvas, message, new RectF(12, 8, Math.Max(1, page.Width - 24), 26));
            OverflowChanged?.Invoke(overflow.OrderBy(row => row.StartUtf16).ToArray());
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

        private bool DrawFitted(ICanvas canvas, string text, RectF area,
            IReadOnlyList<Fragment>? fragments = null, string? gloss = null)
        {
            if (area.Width < 2 || area.Height < 2) return false;
            var font = new Microsoft.Maui.Graphics.Font("Georgia");
            canvas.Font = font;
            canvas.FontColor = Color.FromArgb("#302D27");
            var size = Math.Min(21, area.Height * 0.78f);
            var words = Regex.Matches(text, @"\S+");
            if (words.Count == 0) return false;
            var minimum = fragments is null ? 1 : 7;
            List<WrappedLine> lines;
            while (true)
            {
                if (size < minimum) return false;
                lines = Wrap(size);
                if (lines.Count * size * 1.16f <= area.Height &&
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
                foreach (var fragment in fragments ?? [])
                {
                    var start = Math.Max(fragment.Start, range.Start) - range.Start;
                    var end = Math.Min(fragment.End, range.End) - range.Start;
                    if (start >= end) continue;
                    var bounds = BoundsFor(start, end);
                    hitTargets.Add((bounds, fragment.Source));
                    if (!fragment.Active) continue;
                    canvas.FillColor = Color.FromArgb("#FFF1CF");
                    canvas.FillRectangle(bounds);
                    if (string.IsNullOrWhiteSpace(gloss)) continue;
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

            List<WrappedLine> Wrap(float fontSize)
            {
                var result = new List<WrappedLine>();
                var start = words[0].Index;
                var end = start + words[0].Length;
                for (var i = 1; i < words.Count; i++)
                {
                    var word = words[i];
                    var next = word.Index + word.Length;
                    if (canvas.GetStringSize(text[start..next], font, fontSize).Width > area.Width)
                    {
                        result.Add(new WrappedLine(start, end));
                        start = word.Index;
                    }
                    end = next;
                }
                result.Add(new WrappedLine(start, end));
                return result;
            }
            return true;
        }
    }
}
