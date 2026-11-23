using Reader.Contracts;
using Reader.Native.Controls;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record InlinePdfAddress(string Document, int Page, string? Revision, string? DetachedText);
    private bool showInlineOriginal;
    private string? inlineOriginalDocumentId;
    private bool inlinePdfFitWidth = true;
    private (string Document, int Page, int Start, int End, double SurfaceHeight, double ViewportHeight)? lastInlinePdfFollow;
    private int inlinePdfRequest;
    private InlinePdfAddress? inlinePdfAddress;
    private InlinePdfAddress? inlinePdfLoading;
    private PhotoPreview? inlinePdfPreview;
    private OcrLine[] inlineDetachedLines = [];
    private OcrLine[] inlineNativeBoxes = [];
    private string inlineNativeText = "";

    private async void ToggleInlineOriginalClicked(object? sender, EventArgs args)
    {
        var showOriginal = !showInlineOriginal;
        if (AppState.Session.Document?.Format == "pdf")
            SaveReaderPreference(ReaderPreferenceChange.PdfShowOriginal(showOriginal));
        if (!showOriginal)
        {
            ClearInlineOriginal();
            ScheduleAutoTranslation();
            if (DetachedPdfReader.IsVisible) _ = RestoreDetachedTranslationAsync();
            return;
        }

        showInlineOriginal = true;
        await ShowInlineOriginalAsync();
    }

    private void SyncInlineOriginalMode()
    {
        var session = AppState.Session;
        if (session.Busy) return;
        var document = session.Document;
        var id = session.CanViewOriginal ? document?.Id : null;
        if (id != inlineOriginalDocumentId)
        {
            ClearInlineOriginal();
            inlineOriginalDocumentId = id;
            showInlineOriginal = id is not null &&
                (document?.Format != "pdf" || (readerPreferences.PdfShowOriginal ?? true));
        }
        if (showInlineOriginal) _ = ShowInlineOriginalAsync();
    }

    private void UpdateInlinePdfFitLabel() =>
        InlinePdfFitToggle.Text = inlinePdfFitWidth ? "Ціла сторінка" : "За шириною";

    private void ClearInlineOriginal()
    {
        showInlineOriginal = false;
        inlinePdfRequest++;
        inlinePdfAddress = inlinePdfLoading = null;
        inlinePdfPreview = null;
        pdfSourceLayout = null;
        inlineDetachedLines = inlineNativeBoxes = [];
        inlineNativeText = "";
        InlinePdfHighlight.IsVisible = DetachedInlinePdfHighlight.IsVisible = false;
        InlinePdfHighlight.Drawable = DetachedInlinePdfHighlight.Drawable = null;
        SourceText.IsVisible = DetachedPdfText.IsVisible = true;
        InlinePdfViewport.IsVisible = DetachedInlinePdfViewport.IsVisible = PendingInlinePdfViewport.IsVisible = false;
        InlinePdfFitToggle.IsVisible = false;
        lastInlinePdfFollow = null;
        InlinePdfImage.Source = DetachedInlinePdfImage.Source = PendingInlinePdfImage.Source = null;
        InlinePdfToggle.Text = AppState.Session.Document?.Format == "pdf" ? "Сторінка PDF" : "Фото";
        RefreshPdfTranslationLayout();
    }

    private async Task ShowInlineOriginalAsync()
    {
        var session = AppState.Session;
        if (!showInlineOriginal || session.Document is not { } document)
        {
            ClearInlineOriginal();
            return;
        }

        var request = inlinePdfRequest;
        var isPdf = document.Format == "pdf";
        try
        {
            var page = 0;
            if (isPdf)
            {
                page = CurrentNativePdfWindow(document.Id) is { } window ? window.Center :
                    document.PdfPageStarts is not null
                        ? await session.PdfReadingPageAsync(document.Id) ?? session.OriginalPageIndex()
                        : session.OriginalPageIndex();
            }
            if (request != inlinePdfRequest || !showInlineOriginal) return;
            var detachedText = detachedAddress is { } detached && detached.Document == document.Id &&
                detached.Page == page && DetachedPdfReader.IsVisible ? detached.Text : null;
            var address = new InlinePdfAddress(document.Id, page, document.CurrentRevisionId, detachedText);
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

            InlinePdfToggle.Text = isPdf ? "Завантажуємо PDF…" : "Завантажуємо фото…";
            PhotoPreview preview;
            OcrLine[] detachedLines = [], nativeBoxes = [];
            string nativeText = "";
#if MACCATALYST
            if (isPdf && await session.OriginalFileAsync(document.Id) is { } file)
            {
                if (detachedText is not null)
                    detachedLines = (await session.DetachedPdfOcrAsync(document.Id, page))?.Lines ?? [];
                var canonicalGeometry = detachedText is null &&
                    document.OcrLines?.Any(line => line.Page == page && line.SourceSpan is not null) == true;
                var textStart = document.PdfPageStarts?.ElementAtOrDefault(page)
                    ?? document.Blocks.FirstOrDefault(block => block.Page == page)?.StartUtf16 ?? -1;
                var expectedText = detachedText is not null ? detachedText.AsMemory()
                    : textStart >= 0 && textStart <= document.Text.Length ? document.Text.AsMemory(textStart) : default;
                (preview, nativeText, nativeBoxes) = await PdfScan.ReadingPreviewAsync(file.Path, page,
                    !canonicalGeometry && detachedLines.Length == 0, expectedText);
            }
            else
#endif
            {
                var fileBytes = (await session.OriginalAsync(document.Id)).Bytes.Select(value => checked((byte)value)).ToArray();
                preview = isPdf ? await PdfScan.PreviewAsync(fileBytes, page) : await PhotoOcr.PreviewAsync(fileBytes);
            }

            if (request != inlinePdfRequest || !showInlineOriginal || !readingPageActive ||
                session.Document is not { } current || current.Id != address.Document ||
                current.CurrentRevisionId != address.Revision) return;
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
            ClearInlineOriginal();
            await DisplayAlertAsync(isPdf ? "Сторінка PDF" : "Фото", error.Message, "Гаразд");
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
        if (!showInlineOriginal || inlinePdfPreview is not { Width: > 0, Height: > 0 } preview) return;
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
            if (lastInlinePdfFollow == key && showInlineOriginal && inlinePdfFitWidth)
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
        var activeView = DetachedPdfReader.IsVisible ? DetachedPdfText : SourceText;
        if (ReferenceEquals(sender, activeView) && args.PropertyName == nameof(NativeReaderView.SpokenWord))
            UpdateInlinePdfHighlight(wordOnly: true);
    }

    private void UpdateInlinePdfHighlight(bool wordOnly = false)
    {
        if (!wordOnly) RefreshPdfTranslationLayout();
        if (!showInlineOriginal || inlinePdfAddress is not { } address || inlinePdfPreview is not { } preview ||
            AppState.Session.Document is not { } document || document.Id != address.Document ||
            document.CurrentRevisionId != address.Revision) return;

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
            if (!wordOnly)
            {
                pdfSourceLayout = null;
                DrawPdfTranslationLayout();
            }
            return;
        }

        if (wordOnly)
        {
            if (overlay.IsVisible && overlay.Drawable is PdfReadingHighlight existing &&
                existing.Matches(sentence.StartUtf16, sentence.EndUtf16))
                UpdatePdfWordFocus(existing);
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
        pdfSourceLayout = new(address, lines, offset, sentence);
        DrawPdfTranslationLayout();
        if (!overlay.IsVisible)
        {
            overlay.Drawable = null;
            return;
        }
        var drawing = new PdfReadingHighlight(preview.Width, preview.Height, lines, offset,
            sentence.StartUtf16, sentence.EndUtf16, ReferenceEquals(lines, inlineNativeBoxes));
        overlay.Drawable = drawing;
        UpdatePdfWordFocus(drawing);

        void UpdatePdfWordFocus(PdfReadingHighlight highlight)
        {
            var selected = detached ? (DetachedWordCard.IsVisible ? detachedSelection : null)
                : (WordCard.IsVisible ? selection : null);
            selected ??= DictionaryFocus(document.Id,
                detached ? detachedSentence?.RevisionId : document.CurrentRevisionId,
                sentence, detached ? address.Page : null);
            var spoken = view.SpokenWord;
            highlight.SetFocus(spoken, selected);
            overlay.Invalidate();
            var focus = spoken ?? selected;
            FollowInlinePdf(highlight, document.Id, address.Page, focus?.StartUtf16 ?? sentence.StartUtf16,
                focus is null ? sentence.EndUtf16 : focus.StartUtf16 + focus.LengthUtf16);
        }
    }

    private sealed class PdfReadingHighlight(int width, int height, OcrLine[] lines, int offset,
        int start, int end, bool nativeWords) : IDrawable
    {
        private static readonly Color SentenceFill = Color.FromArgb("#65FFCF4D");
        private static readonly Color SelectionFill = Color.FromArgb("#A5F4A536");
        private static readonly Color SpeechFill = Color.FromArgb("#B5BCD68A");
        private TextSelection? spokenWord;
        private TextSelection? selectedWord;

        internal bool Matches(int sentenceStart, int sentenceEnd) => start == sentenceStart && end == sentenceEnd;

        internal void SetFocus(TextSelection? spoken, TextSelection? selected)
        {
            spokenWord = spoken;
            selectedWord = selected;
        }

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
                    DrawBox(word.X, word.Y, word.Width, word.Height, word.SourceSpan, exactWord: true);
                }
                if (!usedWord) DrawBox(line.X, line.Y, line.Width, line.Height, line.SourceSpan, exactWord: nativeWords);
            }

            void DrawBox(double x, double y, double w, double h, OcrSpan? span, bool exactWord)
            {
                canvas.FillColor = exactWord && IsFocus(span, spokenWord) ? SpeechFill
                    : exactWord && IsFocus(span, selectedWord) ? SelectionFill : SentenceFill;
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
            var focus = spokenWord ?? selectedWord;
            if (focus is not null &&
                RangeBounds(focus.StartUtf16, focus.StartUtf16 + focus.LengthUtf16) is { } word) return word;
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

        private bool IsFocus(OcrSpan? span, TextSelection? focus) =>
            focus is not null && Overlaps(span, focus.StartUtf16, focus.StartUtf16 + focus.LengthUtf16);
    }
}
