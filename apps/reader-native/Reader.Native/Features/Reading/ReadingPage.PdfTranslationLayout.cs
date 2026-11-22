using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record PdfLayoutAddress(string Document, int Page, string? Revision, string Target, string? DetachedText);
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

    private (ScrollView Viewport, GraphicsView Surface) ActivePdfTranslationLayout() =>
        DetachedPdfReader.IsVisible
            ? (DetachedPdfTranslationViewport, DetachedPdfTranslationSurface)
            : (PdfTranslationViewport, PdfTranslationSurface);

    private void TogglePdfTranslationLayout(object? sender, EventArgs args)
    {
        pdfTranslationLayout = !pdfTranslationLayout;
        RefreshPdfTranslationLayout();
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
        (pdfWindow is null || pdfWindow.Center == page.Page)
            ? new(page.Document, page.Page, document.CurrentRevisionId, target, page.DetachedText)
            : null;

    private void RefreshPdfTranslationLayout()
    {
        var address = CurrentPdfLayoutAddress();
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
            pdfLayoutRows = [];
            pdfLayoutGeometry = [];
            pdfLayoutSentence = null;
            pdfLayoutStatus = null;
            if (shown && address is not null) _ = LoadPdfPageLayoutAsync(address, cachedOnly: true);
        }
        else if (shown && address is not null && pdfLayoutRows.Length == 0 && pdfLayoutStatus is null)
            _ = LoadPdfPageLayoutAsync(address, cachedOnly: true);
        if (shown)
        {
            ResizePdfTranslationLayout();
            DrawPdfTranslationLayout();
        }
    }

    private async Task LoadPdfPageLayoutAsync(PdfLayoutAddress address, bool cachedOnly)
    {
        var request = ++pdfLayoutRequestId;
        pdfLayoutStatus = cachedOnly ? "Завантажуємо переклад…" : "Перекладаємо сторінку…";
        if (!cachedOnly) translatingPdfLayout = address;
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

    private async void RefreshCachedPdfPageLayout()
    {
        if (refreshingPdfLayout || !pdfTranslationLayout || translatingPdfLayout is not null ||
            CurrentPdfLayoutAddress() is not { } address) return;
        refreshingPdfLayout = true;
        try
        {
            await Task.Delay(150);
            if (CurrentPdfLayoutAddress() == address && translatingPdfLayout is null)
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
        var target = Math.Clamp(source.ScrollY * destinationHeight / sourceHeight, 0,
            Math.Max(0, destinationHeight - destination.Height));
        if (Math.Abs(destination.ScrollY - target) < 1) return;

        syncingPdfTranslationScroll = true;
        try { await destination.ScrollToAsync(0, target, animated: false); }
        catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
        finally { syncingPdfTranslationScroll = false; }
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
        surface.Drawable = new TranslatedPdfDrawing(pdfLayoutRows, pdfLayoutGeometry, pdfLayoutOffset,
            pdfLayoutSentence, gloss, pdfLayoutStatus, background.IsVisible);
        SemanticProperties.SetDescription(surface, string.Join(" ", pdfLayoutRows.Select(row => row.Text).Where(text => text is not null)));
        surface.Invalidate();
    }

    private sealed class TranslatedPdfDrawing(PdfPageTranslationRow[] rows, OcrLine[] geometry, int offset,
        Sentence? active, string? gloss, string? status, bool preserveArtwork) : IDrawable
    {
        private sealed record Region(RectF Bounds, List<PdfPageTranslationRow> Rows);

        public void Draw(ICanvas canvas, RectF page)
        {
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
            foreach (var row in rows)
            {
                var bounds = Bounds(row, page);
                if (bounds is not { } rect) continue;
                var index = regions.FindLastIndex(region =>
                    region.Bounds.Top + 1 < rect.Bottom && rect.Top + 1 < region.Bounds.Bottom &&
                    region.Bounds.Left < rect.Right && rect.Left < region.Bounds.Right);
                if (index >= 0)
                {
                    var previous = regions[index];
                    previous.Rows.Add(row);
                    regions[index] = previous with { Bounds = Union(previous.Bounds, rect) };
                }
                else regions.Add(new(rect, [row]));
            }

            foreach (var region in regions)
            {
                var text = string.Join(" ", region.Rows.Where(row => row.Text is not null).Select(row => row.Text));
                var highlighted = active is not null && region.Rows.Any(row =>
                    row.StartUtf16 < active.EndUtf16 && row.EndUtf16 > active.StartUtf16);
                if (highlighted)
                {
                    canvas.FillColor = Color.FromArgb("#FFF1CF");
                    canvas.FillRoundedRectangle(region.Bounds, 3);
                }
                if (text.Length > 0) DrawFitted(canvas, text, region.Bounds, highlighted ? gloss : null);
            }

            var message = !string.IsNullOrWhiteSpace(status) ? status :
                regions.Count == 0 && rows.Any(row => row.Text is not null)
                    ? "Координати тексту недоступні. Перемкніться на список." : null;
            if (message is not null)
                DrawFitted(canvas, message, new RectF(12, 8, Math.Max(1, page.Width - 24), 26));
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

        private static void DrawFitted(ICanvas canvas, string text, RectF area, string? gloss = null)
        {
            if (area.Width < 2 || area.Height < 2) return;
            var font = new Microsoft.Maui.Graphics.Font("Georgia");
            canvas.Font = font;
            canvas.FontColor = Color.FromArgb("#302D27");
            var size = Math.Min(21, area.Height * 0.78f);
            List<string> lines;
            do
            {
                lines = Wrap(size);
                if (lines.Count * size * 1.16f <= area.Height &&
                    lines.All(line => canvas.GetStringSize(line, font, size).Width <= area.Width)) break;
                size *= 0.84f;
            } while (size > 1);
            canvas.FontSize = size;
            var y = area.Top;
            foreach (var line in lines)
            {
                if (!string.IsNullOrWhiteSpace(gloss))
                {
                    var index = line.IndexOf(gloss, StringComparison.OrdinalIgnoreCase);
                    var end = index + gloss.Length;
                    if (index >= 0 && (index == 0 || !char.IsLetterOrDigit(line[index - 1])) &&
                        (end == line.Length || !char.IsLetterOrDigit(line[end])))
                    {
                        var left = canvas.GetStringSize(line[..index], font, size).Width;
                        var width = canvas.GetStringSize(line[index..end], font, size).Width;
                        canvas.FillColor = Color.FromArgb("#FFE48B");
                        canvas.FillRectangle(area.Left + left, y, width, size * 1.16f);
                    }
                }
                canvas.DrawString(line, area.Left, y, area.Width, size * 1.16f,
                    HorizontalAlignment.Left, VerticalAlignment.Center);
                y += size * 1.16f;
            }

            List<string> Wrap(float fontSize)
            {
                var result = new List<string>();
                var current = "";
                foreach (var word in text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries))
                {
                    var combined = current.Length == 0 ? word : $"{current} {word}";
                    if (current.Length > 0 && canvas.GetStringSize(combined, font, fontSize).Width > area.Width)
                    {
                        result.Add(current);
                        current = word;
                    }
                    else current = combined;
                }
                if (current.Length > 0) result.Add(current);
                return result;
            }
        }
    }
}
