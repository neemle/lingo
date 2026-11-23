using Reader.Contracts;
using Reader.Native.Controls;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record InlinePdfAddress(string Document, int Page, string? Revision, string? DetachedText);
    private sealed record PdfWordAnchor(VisualElement View, InlinePdfAddress Address, Rect Bounds);
    private enum PdfFit { PageWidth, TextWidth, WholePage }
    private bool showInlineOriginal;
    private string? inlineOriginalDocumentId;
    private PdfFit inlinePdfFit;
    private bool PdfFollowsScroll => inlinePdfFit != PdfFit.WholePage;
    private (string Document, int Page, int Start, int End, double SurfaceHeight, double ViewportHeight)? lastInlinePdfFollow;
    private int followingInlinePdf;
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

    private void UpdateInlinePdfFitLabel()
    {
        InlinePdfFitToggle.Text = inlinePdfFit switch
        {
            PdfFit.TextWidth when AppState.Session.Document?.Format == "pdf" => "За шириною тексту",
            PdfFit.WholePage => "Ціла сторінка",
            _ => "За шириною сторінки"
        };
        SemanticProperties.SetDescription(InlinePdfFitToggle, $"Масштаб. {InlinePdfFitToggle.Text}. Натисніть, щоб змінити.");
    }

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
            var changedPage = inlinePdfAddress is not { } previous ||
                previous.Document != address.Document || previous.Page != address.Page;
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
            UpdateInlinePdfFitLabel();
            InlinePdfToggle.Text = "Розпізнаний текст";
            ResizeInlinePdf();
            var viewport = ActiveInlinePdf().Viewport;
            if (changedPage && viewport.ScrollY >= 1)
            {
                try { await viewport.ScrollToAsync(0, 0, animated: false).WaitAsync(TimeSpan.FromMilliseconds(350)); }
                catch (Exception error) when (error is InvalidOperationException or OperationCanceledException or TimeoutException) { }
            }
            if (request != inlinePdfRequest || !readingPageActive) return;
            lastInlinePdfFollow = null;
            UpdateInlinePdfHighlight();
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
        inlinePdfFit = inlinePdfFit switch
        {
            PdfFit.PageWidth when AppState.Session.Document?.Format == "pdf" => PdfFit.TextWidth,
            PdfFit.WholePage => PdfFit.PageWidth,
            _ => PdfFit.WholePage
        };
        SaveReaderPreference(ReaderPreferenceChange.PdfFit(inlinePdfFit switch
        {
            PdfFit.TextWidth => "textWidth",
            PdfFit.WholePage => "wholePage",
            _ => "pageWidth"
        }));
        UpdateInlinePdfFitLabel();
        lastInlinePdfFollow = null;
        lastPdfTranslationFollow = null;
        ResizeInlinePdf();
        UpdateInlinePdfHighlight();
    }

    private (ScrollView Viewport, Grid Surface) ActiveInlinePdf() =>
        DetachedPdfReader.IsVisible ? (DetachedInlinePdfViewport, DetachedInlinePdfSurface) :
        PdfPageNotice.IsVisible ? (PendingInlinePdfViewport, PendingInlinePdfSurface) :
        (InlinePdfViewport, InlinePdfSurface);

    private void InlinePdfViewportChanged(object? sender, EventArgs args) => ResizeInlinePdf();

    private void InlinePdfSurfaceChanged(object? sender, EventArgs args)
    {
        UpdateInlinePdfHighlight();
        PositionWordPopup();
    }

    private void ResizeInlinePdf()
    {
        if (!showInlineOriginal) return;
        var (viewport, surface) = ActiveInlinePdf();
        ResizePdfPaper(viewport, surface);
        ResizePdfTranslationLayout();
    }

    private void ResizePdfPaper(ScrollView viewport, Grid paper)
    {
        if (inlinePdfPreview is not { Width: > 0, Height: > 0 } preview ||
            paper.Parent is not AbsoluteLayout frame || viewport.Width < 1 || viewport.Height < 1) return;
        var width = Math.Floor(viewport.Width);
        var fitWidth = width;
        var fitHeight = viewport.Height;
        if (PdfPapersSideBySide(DetachedPdfReader.IsVisible))
        {
            var (source, _) = ActiveInlinePdf();
            var (target, _) = ActivePdfTranslationLayout();
            if (source.Width >= 1 && target.Width >= 1 && source.Height >= 1 && target.Height >= 1)
            {
                fitWidth = Math.Floor(Math.Min(source.Width, target.Width));
                fitHeight = Math.Min(source.Height, target.Height);
            }
        }
        var scale = fitWidth / preview.Width;
        var center = 0.5;
        if (inlinePdfFit == PdfFit.WholePage)
            scale = Math.Min(scale, fitHeight / preview.Height);
        else
        {
            var (left, right) = PdfReadingHorizontalBounds();
            scale /= right - left;
            center = (left + right) / 2;
        }

        var paperWidth = Math.Floor(preview.Width * scale);
        var paperHeight = Math.Floor(preview.Height * scale);
        var bounds = new Rect(width / 2 - paperWidth * center, 0, paperWidth, paperHeight);
        if (AbsoluteLayout.GetLayoutBounds(paper) != bounds)
            AbsoluteLayout.SetLayoutBounds(paper, bounds);
        frame.WidthRequest = width;
        frame.HeightRequest = paperHeight;
    }

    private (double Left, double Right) PdfReadingHorizontalBounds() =>
        inlinePdfFit == PdfFit.TextWidth && AppState.Session.Document?.Format == "pdf" &&
        pdfSourceLayout is { } source && source.Page == inlinePdfAddress
            ? PdfTextBounds(source.Lines) : (0, 1);

    private static (double Left, double Right) PdfTextBounds(OcrLine[] lines)
    {
        var left = 1d;
        var right = 0d;
        foreach (var line in lines)
        {
            if (string.IsNullOrWhiteSpace(line.Text) || !double.IsFinite(line.X) ||
                !double.IsFinite(line.Width) || line.Width <= 0) continue;
            left = Math.Min(left, Math.Clamp(line.X, 0, 1));
            right = Math.Max(right, Math.Clamp(line.X + line.Width, 0, 1));
        }

        if (right <= left) return (0, 1);
        const double padding = 0.025;
        // A short line on a sparse title page should not cause extreme zoom.
        var center = Math.Clamp((left + right) / 2, 0.25, 0.75);
        return (Math.Max(0, Math.Min(left - padding, center - 0.25)),
            Math.Min(1, Math.Max(right + padding, center + 0.25)));
    }

    private void FollowInlinePdf(PdfReadingHighlight drawing, string document, int page, int start, int end)
    {
        if (!PdfFollowsScroll || drawing.FocusBounds() is not { } bounds) return;
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
            if (lastInlinePdfFollow != key || !showInlineOriginal || !PdfFollowsScroll) return;
            followingInlinePdf++;
            try { await viewport.ScrollToAsync(0, target, animated: false); }
            catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
            finally { followingInlinePdf--; }
        });
    }

    private async void InlinePdfTapped(object? sender, TappedEventArgs args)
    {
        var detached = ReferenceEquals(sender, DetachedInlinePdfImage);
        var view = detached ? DetachedInlinePdfImage : InlinePdfImage;
        var overlay = detached ? DetachedInlinePdfHighlight : InlinePdfHighlight;
        var source = detached ? detachedAddress?.Text : AppState.Session.Document?.Text;
        if (source is null || inlinePdfAddress is not { } address ||
            !overlay.IsVisible || overlay.Drawable is not PdfReadingHighlight drawing ||
            args.GetPosition(view) is not { } point ||
            drawing.HitAt(source.Length, point, (float)view.Width, (float)view.Height) is not { } hit) return;

        if (!hit.IsWord)
        {
            await SelectPdfSourceRangeAsync(address, hit.Start, hit.End);
            return;
        }

        if (SourceRanges.Selection(source, hit.Start, hit.End - hit.Start) is not { } selected) return;
        var anchor = new PdfWordAnchor(view, address, hit.Bounds);
        if (detached)
        {
            await ShowDetachedSelectionAsync(selected, anchor);
        }
        else
        {
            await ShowWordAsync(selected, anchor);
        }
    }

    private (int Start, int End)? VisibleInlinePdfOffsets()
    {
        var (viewport, surface) = ActiveInlinePdf();
        var overlay = DetachedPdfReader.IsVisible ? DetachedInlinePdfHighlight : InlinePdfHighlight;
        if (!PdfFollowsScroll || !viewport.IsVisible || surface.Height <= 0 ||
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
        if (detached != (address.DetachedText is not null) ||
            (detached && detachedAddress?.Page != address.Page))
        {
            overlay.IsVisible = false;
            if (!wordOnly)
            {
                pdfSourceLayout = null;
                if (inlinePdfFit == PdfFit.TextWidth) ResizeInlinePdf();
                DrawPdfTranslationLayout();
            }
            return;
        }

        if (wordOnly)
        {
            if (sentence is not null && overlay.IsVisible && overlay.Drawable is PdfReadingHighlight existing &&
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

        overlay.IsVisible = lines.Length > 0 && sentence is not null;
        pdfSourceLayout = new(address, lines, offset, sentence);
        if (inlinePdfFit == PdfFit.TextWidth) ResizeInlinePdf();
        DrawPdfTranslationLayout();
        if (!overlay.IsVisible || sentence is null)
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
            selected ??= FindSourceFocus(document.Id,
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

        internal (int Start, int End, Rect Bounds, bool IsWord)? HitAt(
            int sourceLength, Point point, float viewWidth, float viewHeight)
        {
            var fit = OriginalPage.AspectFit(new RectF(0, 0, viewWidth, viewHeight), width, height);
            if (fit.Width <= 0 || fit.Height <= 0) return null;
            var x = (point.X - fit.X) / fit.Width;
            var y = (point.Y - fit.Y) / fit.Height;
            (int Start, int End, Rect Bounds, bool IsWord)? lineHit = null;
            foreach (var line in lines)
            {
                if (x >= line.X && x <= line.X + line.Width &&
                    y >= line.Y && y <= line.Y + line.Height && line.SourceSpan is { } lineSpan)
                {
                    var bounds = new Rect(line.X, line.Y, line.Width, line.Height);
                    var candidate = ToHit(lineSpan, bounds, isWord: nativeWords);
                    if (nativeWords && candidate is not null) return candidate;
                    lineHit ??= candidate;
                }

                foreach (var word in line.Words ?? [])
                {
                    if (x >= word.X && x <= word.X + word.Width && y >= word.Y && y <= word.Y + word.Height &&
                        word.SourceSpan is { } span &&
                        ToHit(span, new Rect(word.X, word.Y, word.Width, word.Height), isWord: true) is { } hit)
                        return hit;
                }
            }
            return lineHit;

            (int Start, int End, Rect Bounds, bool IsWord)? ToHit(OcrSpan span, Rect bounds, bool isWord)
            {
                var first = span.StartUtf16 + offset;
                var last = span.EndUtf16 + offset;
                return first >= 0 && first < last && last <= sourceLength
                    ? (first, last, bounds, isWord) : null;
            }
        }

        private bool Overlaps(OcrSpan? span, int rangeStart, int rangeEnd) =>
            span is { } box && box.StartUtf16 + offset < rangeEnd && box.EndUtf16 + offset > rangeStart;

        private bool IsFocus(OcrSpan? span, TextSelection? focus) =>
            focus is not null && Overlaps(span, focus.StartUtf16, focus.StartUtf16 + focus.LengthUtf16);
    }
}
