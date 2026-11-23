using Reader.Contracts;
using Reader.Native.Controls;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool inlinePdf;
    private string? inlinePdfDocumentId;
    private bool inlinePdfFitWidth = true;
    private (string Document, int Page, int Start, int End, double SurfaceHeight, double ViewportHeight)? lastInlinePdfFollow;
    private int inlinePdfRequest;
    private (string Document, int Page, string? DetachedText)? inlinePdfAddress;
    private (string Document, int Page, string? DetachedText)? inlinePdfLoading;
    private PhotoPreview? inlinePdfPreview;
    private OcrLine[] inlineDetachedLines = [];
    private OcrLine[] inlineNativeBoxes = [];
    private string inlineNativeText = "";

    private async void ToggleInlinePdfClicked(object? sender, EventArgs args)
    {
        var showOriginal = !inlinePdf;
        SaveReaderPreference(ReaderPreferenceChange.PdfShowOriginal(showOriginal));
        if (!showOriginal)
        {
            ClearInlinePdf();
            ScheduleAutoTranslation();
            if (DetachedPdfReader.IsVisible) _ = RestoreDetachedTranslationAsync();
            return;
        }

        inlinePdf = true;
        await ShowInlinePdfAsync();
    }

    private void SyncInlinePdfMode()
    {
        var session = AppState.Session;
        if (session.Busy) return;
        var id = session.Document is { Format: "pdf" } document ? document.Id : null;
        if (id != inlinePdfDocumentId)
        {
            ClearInlinePdf();
            inlinePdfDocumentId = id;
            inlinePdf = id is not null && (readerPreferences.PdfShowOriginal ?? true);
        }
        if (inlinePdf) _ = ShowInlinePdfAsync();
    }

    private void UpdateInlinePdfFitLabel() =>
        InlinePdfFitToggle.Text = inlinePdfFitWidth ? "Ціла сторінка" : "За шириною";

    private void ClearInlinePdf()
    {
        inlinePdf = false;
        inlinePdfRequest++;
        inlinePdfAddress = inlinePdfLoading = null;
        inlinePdfPreview = null;
        inlineDetachedLines = inlineNativeBoxes = [];
        inlineNativeText = "";
        InlinePdfHighlight.IsVisible = DetachedInlinePdfHighlight.IsVisible = false;
        InlinePdfHighlight.Drawable = DetachedInlinePdfHighlight.Drawable = null;
        SourceText.IsVisible = DetachedPdfText.IsVisible = true;
        InlinePdfViewport.IsVisible = DetachedInlinePdfViewport.IsVisible = PendingInlinePdfViewport.IsVisible = false;
        InlinePdfFitToggle.IsVisible = false;
        lastInlinePdfFollow = null;
        InlinePdfImage.Source = DetachedInlinePdfImage.Source = PendingInlinePdfImage.Source = null;
        InlinePdfToggle.Text = "Сторінка PDF";
        RefreshPdfTranslationLayout();
    }

    private async Task ShowInlinePdfAsync()
    {
        var session = AppState.Session;
        if (!inlinePdf || session.Document is not { Format: "pdf" } document)
        {
            ClearInlinePdf();
            return;
        }

        var request = inlinePdfRequest;
        try
        {
            var page = CurrentNativePdfWindow(document.Id) is { } window ? window.Center :
                document.PdfPageStarts is not null
                    ? await session.PdfReadingPageAsync(document.Id) ?? session.OriginalPageIndex()
                    : session.OriginalPageIndex();
            if (request != inlinePdfRequest || !inlinePdf) return;
            var detachedText = detachedAddress is { } detached && detached.Document == document.Id &&
                detached.Page == page && DetachedPdfReader.IsVisible ? detached.Text : null;
            var address = (document.Id, page, detachedText);
            if (inlinePdfAddress == address)
            {
                if (inlinePdfLoading is not null && inlinePdfLoading != address)
                {
                    inlinePdfRequest++;
                    inlinePdfLoading = null;
                }
                InlinePdfToggle.Text = "Розпізнаний текст";
                PendingInlinePdfViewport.IsVisible = PdfPageNotice.IsVisible && !DetachedPdfReader.IsVisible;
                ResizeInlinePdf();
                UpdateInlinePdfHighlight();
                return;
            }
            if (inlinePdfLoading == address) return;
            request = ++inlinePdfRequest;
            inlinePdfLoading = address;
            InlinePdfHighlight.IsVisible = DetachedInlinePdfHighlight.IsVisible = false;

            InlinePdfToggle.Text = "Завантажуємо PDF…";
            PhotoPreview preview;
            OcrLine[] detachedLines = [], nativeBoxes = [];
            string nativeText = "";
#if MACCATALYST
            if (await session.OriginalFileAsync(document.Id) is { } file)
            {
                if (detachedText is not null)
                    detachedLines = (await session.DetachedPdfOcrAsync(document.Id, page))?.Lines ?? [];
                var canonicalGeometry = detachedText is null &&
                    document.OcrLines?.Any(line => line.Page == page && line.SourceSpan is not null) == true;
                (preview, nativeText, nativeBoxes) = await PdfScan.ReadingPreviewAsync(file.Path, page,
                    !canonicalGeometry && detachedLines.Length == 0);
            }
            else
#endif
            {
                var fileBytes = (await session.OriginalAsync(document.Id)).Bytes.Select(value => checked((byte)value)).ToArray();
                preview = await PdfScan.PreviewAsync(fileBytes, page);
            }

            if (request != inlinePdfRequest || !inlinePdf || !readingPageActive ||
                session.Document?.Id != document.Id) return;
            var source = ImageSource.FromStream(() => new MemoryStream(preview.Bytes, writable: false));
            InlinePdfImage.Source = DetachedInlinePdfImage.Source = PendingInlinePdfImage.Source = source;
            InlinePdfViewport.IsVisible = DetachedInlinePdfViewport.IsVisible = true;
            PendingInlinePdfViewport.IsVisible = PdfPageNotice.IsVisible && !DetachedPdfReader.IsVisible;
            SourceText.IsVisible = DetachedPdfText.IsVisible = false;
            inlinePdfAddress = address;
            inlinePdfLoading = null;
            inlinePdfPreview = preview;
            inlineDetachedLines = detachedLines;
            inlineNativeBoxes = nativeBoxes;
            inlineNativeText = nativeText;
            lastInlinePdfFollow = null;
            InlinePdfFitToggle.IsVisible = true;
            ResizeInlinePdf();
            await ActiveInlinePdf().Viewport.ScrollToAsync(0, 0, animated: false);
            if (request != inlinePdfRequest) return;
            lastInlinePdfFollow = null;
            UpdateInlinePdfHighlight();
            InlinePdfToggle.Text = "Розпізнаний текст";
        }
        catch (Exception error)
        {
            if (request != inlinePdfRequest) return;
            ClearInlinePdf();
            await DisplayAlertAsync("Сторінка PDF", error.Message, "Гаразд");
        }
    }

    private void ToggleInlinePdfFitClicked(object? sender, EventArgs args)
    {
        inlinePdfFitWidth = !inlinePdfFitWidth;
        SaveReaderPreference(ReaderPreferenceChange.PdfFitWidth(inlinePdfFitWidth));
        UpdateInlinePdfFitLabel();
        lastInlinePdfFollow = null;
        ResizeInlinePdf();
        UpdateInlinePdfHighlight();
    }

    private (ScrollView Viewport, Grid Surface) ActiveInlinePdf() =>
        DetachedPdfReader.IsVisible ? (DetachedInlinePdfViewport, DetachedInlinePdfSurface) :
        PdfPageNotice.IsVisible ? (PendingInlinePdfViewport, PendingInlinePdfSurface) :
        (InlinePdfViewport, InlinePdfSurface);

    private void InlinePdfViewportChanged(object? sender, EventArgs args) => ResizeInlinePdf();

    private void InlinePdfSurfaceChanged(object? sender, EventArgs args) => UpdateInlinePdfHighlight();

    private void ResizeInlinePdf()
    {
        if (!inlinePdf || inlinePdfPreview is not { Width: > 0, Height: > 0 } preview) return;
        var (viewport, surface) = ActiveInlinePdf();
        if (viewport.Width <= 0 || viewport.Height <= 0) return;
        var widthScale = viewport.Width / preview.Width;
        var scale = inlinePdfFitWidth ? widthScale : Math.Min(widthScale, viewport.Height / preview.Height);
        surface.WidthRequest = Math.Floor(preview.Width * scale);
        surface.HeightRequest = Math.Floor(preview.Height * scale);
        ResizePdfTranslationLayout();
    }

    private void FollowInlinePdf(PdfReadingHighlight drawing, string document, int page, int start, int end)
    {
        if (!inlinePdfFitWidth || drawing.FocusBounds() is not { } bounds) return;
        var (viewport, surface) = ActiveInlinePdf();
        if (surface.Height <= 0 || viewport.Height <= 0) return;
        var key = (document, page, start, end, surface.Height, viewport.Height);
        if (lastInlinePdfFollow == key) return;
        lastInlinePdfFollow = key;
        var top = bounds.Top * surface.Height;
        var bottom = bounds.Bottom * surface.Height;
        var margin = Math.Min(viewport.Height * 0.12, 44);
        if (top >= viewport.ScrollY + margin && bottom <= viewport.ScrollY + viewport.Height - margin) return;
        var target = Math.Clamp(top - margin, 0, Math.Max(0, surface.Height - viewport.Height));
        Dispatcher.Dispatch(async () =>
        {
            if (lastInlinePdfFollow == key && inlinePdf && inlinePdfFitWidth)
                await viewport.ScrollToAsync(0, target, animated: false);
        });
    }

    private async void InlinePdfTapped(object? sender, TappedEventArgs args)
    {
        var detached = ReferenceEquals(sender, DetachedInlinePdfImage);
        var view = detached ? DetachedInlinePdfImage : InlinePdfImage;
        var overlay = detached ? DetachedInlinePdfHighlight : InlinePdfHighlight;
        var source = detached ? detachedAddress?.Text : AppState.Session.Document?.Text;
        if (source is null || !overlay.IsVisible || overlay.Drawable is not PdfReadingHighlight drawing ||
            args.GetPosition(view) is not { } point ||
            drawing.SelectionAt(source, point, (float)view.Width, (float)view.Height) is not { } selected) return;

        if (detached)
        {
            await ShowDetachedSelectionAsync(selected, (view, point));
        }
        else
        {
            await ShowWordAsync(selected, (view, point));
        }
    }

    private (int Start, int End)? VisibleInlinePdfOffsets()
    {
        var (viewport, surface) = ActiveInlinePdf();
        var overlay = DetachedPdfReader.IsVisible ? DetachedInlinePdfHighlight : InlinePdfHighlight;
        if (!inlinePdfFitWidth || !viewport.IsVisible || surface.Height <= 0 ||
            overlay.Drawable is not PdfReadingHighlight drawing) return null;
        var top = viewport.ScrollY / surface.Height;
        return drawing.VisibleOffsets(top, top + viewport.Height / surface.Height);
    }

    private void InlinePdfWordChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(NativeReaderView.SpokenWord)) UpdateInlinePdfHighlight();
    }

    private void UpdateInlinePdfHighlight()
    {
        RefreshPdfTranslationLayout();
        if (!inlinePdf || inlinePdfAddress is not { } address || inlinePdfPreview is not { } preview ||
            AppState.Session.Document is not { } document || document.Id != address.Document) return;

        var detached = DetachedPdfReader.IsVisible;
        var overlay = detached ? DetachedInlinePdfHighlight : InlinePdfHighlight;
        var other = detached ? InlinePdfHighlight : DetachedInlinePdfHighlight;
        other.IsVisible = false;
        var view = detached ? DetachedPdfText : SourceText;
        var sentence = detached ? detachedSentence?.Sentence : AppState.Session.Sentence;
        if (sentence is null || detached != (address.DetachedText is not null) ||
            (detached && detachedAddress?.Page != address.Page))
        {
            overlay.IsVisible = false;
            return;
        }

        var lines = detached ? inlineDetachedLines :
            document.OcrLines?.Where(line => line.Page == address.Page && line.SourceSpan is not null).ToArray() ?? [];
        var offset = 0;
        if (lines.Length == 0 && inlineNativeBoxes.Length > 0)
        {
            var source = address.DetachedText ?? document.Text;
            var start = detached ? 0 : document.PdfPageStarts?.ElementAtOrDefault(address.Page)
                ?? document.Blocks.FirstOrDefault(block => block.Page == address.Page)?.StartUtf16 ?? -1;
            if (start >= 0 && start <= source.Length - inlineNativeText.Length &&
                source.AsSpan(start, inlineNativeText.Length).SequenceEqual(inlineNativeText))
            {
                lines = inlineNativeBoxes;
                offset = start;
            }
        }

        overlay.IsVisible = lines.Length > 0;
        pdfLayoutGeometry = lines;
        pdfLayoutOffset = offset;
        pdfLayoutSentence = sentence;
        DrawPdfTranslationLayout();
        if (!overlay.IsVisible)
        {
            overlay.Drawable = null;
            return;
        }
        var selected = detached ? (DetachedWordCard.IsVisible ? detachedSelection : null)
            : (WordCard.IsVisible ? selection : null);
        var dictionarySelection = DictionaryFocus(document.Id,
            detached ? detachedSentence?.RevisionId : document.CurrentRevisionId,
            sentence, detached ? address.Page : null);
        var focus = selected ?? view.SpokenWord ?? dictionarySelection;
        var drawing = new PdfReadingHighlight(preview.Width, preview.Height, lines, offset,
            sentence.StartUtf16, sentence.EndUtf16, focus?.StartUtf16, focus?.LengthUtf16 ?? 0,
            ReferenceEquals(lines, inlineNativeBoxes));
        overlay.Drawable = drawing;
        overlay.Invalidate();
        FollowInlinePdf(drawing, document.Id, address.Page, focus?.StartUtf16 ?? sentence.StartUtf16,
            focus is null ? sentence.EndUtf16 : focus.StartUtf16 + focus.LengthUtf16);
    }

    private sealed class PdfReadingHighlight(int width, int height, OcrLine[] lines, int offset,
        int start, int end, int? spokenStart, int spokenLength, bool nativeWords) : IDrawable
    {
        public void Draw(ICanvas canvas, RectF area)
        {
            var fitted = OriginalPage.AspectFit(area, width, height);
            if (fitted.Width <= 0 || fitted.Height <= 0) return;
            foreach (var line in lines)
            {
                if (!Overlaps(line.SourceSpan, start, end)) continue;
                var usedWord = false;
                foreach (var word in line.Words ?? [])
                {
                    if (!Overlaps(word.SourceSpan, start, end)) continue;
                    usedWord = true;
                    DrawBox(word.X, word.Y, word.Width, word.Height, word.SourceSpan);
                }
                if (!usedWord) DrawBox(line.X, line.Y, line.Width, line.Height, line.SourceSpan);
            }

            void DrawBox(double x, double y, double w, double h, OcrSpan? span)
            {
                var speaking = spokenStart is int wordStart &&
                    Overlaps(span, wordStart, wordStart + spokenLength);
                canvas.FillColor = Color.FromArgb(speaking ? "#A5F4A536" : "#65FFCF4D");
                canvas.FillRoundedRectangle(fitted.X + (float)x * fitted.Width,
                    fitted.Y + (float)y * fitted.Height, (float)w * fitted.Width, (float)h * fitted.Height, 2);
            }
        }

        internal (int Start, int End)? VisibleOffsets(double top, double bottom)
        {
            int first = int.MaxValue, last = 0;
            foreach (var line in lines)
            {
                if (line.Y + line.Height <= top || line.Y >= bottom || line.SourceSpan is not { } span) continue;
                first = Math.Min(first, offset + span.StartUtf16);
                last = Math.Max(last, offset + span.EndUtf16);
            }
            return first < last ? (first, last) : null;
        }

        internal (double Top, double Bottom)? FocusBounds()
        {
            if (spokenStart is int wordStart && RangeBounds(wordStart, wordStart + spokenLength) is { } word)
                return word;
            return RangeBounds(start, end);
        }

        private (double Top, double Bottom)? RangeBounds(int first, int last)
        {
            double top = 1, bottom = 0;
            foreach (var line in lines)
            {
                if (!Overlaps(line.SourceSpan, first, last)) continue;
                var matched = false;
                foreach (var word in line.Words ?? [])
                {
                    if (!Overlaps(word.SourceSpan, first, last)) continue;
                    top = Math.Min(top, word.Y);
                    bottom = Math.Max(bottom, word.Y + word.Height);
                    matched = true;
                }
                if (matched) continue;
                top = Math.Min(top, line.Y);
                bottom = Math.Max(bottom, line.Y + line.Height);
            }
            return top < bottom ? (top, bottom) : null;
        }

        internal TextSelection? SelectionAt(string source, Point point, float viewWidth, float viewHeight)
        {
            var fit = OriginalPage.AspectFit(new RectF(0, 0, viewWidth, viewHeight), width, height);
            if (fit.Width <= 0 || fit.Height <= 0) return null;
            var x = (point.X - fit.X) / fit.Width;
            var y = (point.Y - fit.Y) / fit.Height;
            foreach (var line in lines)
            {
                if (nativeWords && x >= line.X && x <= line.X + line.Width &&
                    y >= line.Y && y <= line.Y + line.Height && line.SourceSpan is { } nativeSpan)
                    return ToSelection(nativeSpan);

                foreach (var word in line.Words ?? [])
                {
                    if (x >= word.X && x <= word.X + word.Width && y >= word.Y && y <= word.Y + word.Height &&
                        word.SourceSpan is { } span)
                        return ToSelection(span);
                }
            }
            return null;

            TextSelection? ToSelection(OcrSpan span) =>
                SourceRanges.Selection(source, span.StartUtf16 + offset, span.EndUtf16 - span.StartUtf16);
        }

        private bool Overlaps(OcrSpan? span, int rangeStart, int rangeEnd) =>
            span is { } box && box.StartUtf16 + offset < rangeEnd && box.EndUtf16 + offset > rangeStart;
    }
}
