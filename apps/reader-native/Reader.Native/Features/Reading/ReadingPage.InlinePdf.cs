using Reader.Contracts;
using Reader.Native.Controls;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool inlinePdf;
    private int inlinePdfRequest;
    private (string Document, int Page, string? DetachedText)? inlinePdfAddress;
    private (string Document, int Page, string? DetachedText)? inlinePdfLoading;
    private PhotoPreview? inlinePdfPreview;
    private OcrLine[] inlineDetachedLines = [];
    private OcrLine[] inlineNativeBoxes = [];
    private string inlineNativeText = "";

    private async void ToggleInlinePdfClicked(object? sender, EventArgs args)
    {
        if (inlinePdf)
        {
            ClearInlinePdf();
            return;
        }

        inlinePdf = true;
        await ShowInlinePdfAsync();
    }

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
        InlinePdfImage.IsVisible = DetachedInlinePdfImage.IsVisible = PendingInlinePdfImage.IsVisible = false;
        InlinePdfImage.Source = DetachedInlinePdfImage.Source = PendingInlinePdfImage.Source = null;
        InlinePdfToggle.Text = "Сторінка PDF";
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
            var page = pdfSnapshot?.Id == document.Id && pdfWindow is { } window ? window.Center :
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
                PendingInlinePdfImage.IsVisible = PdfPageNotice.IsVisible && !DetachedPdfReader.IsVisible;
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
            InlinePdfImage.IsVisible = DetachedInlinePdfImage.IsVisible = true;
            PendingInlinePdfImage.IsVisible = PdfPageNotice.IsVisible && !DetachedPdfReader.IsVisible;
            SourceText.IsVisible = DetachedPdfText.IsVisible = false;
            inlinePdfAddress = address;
            inlinePdfLoading = null;
            inlinePdfPreview = preview;
            inlineDetachedLines = detachedLines;
            inlineNativeBoxes = nativeBoxes;
            inlineNativeText = nativeText;
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
            if (detachedAddress is not { } address) return;
            if (detachedSentence is not { } current || selected.StartUtf16 < current.Sentence.StartUtf16 ||
                selected.StartUtf16 >= current.Sentence.EndUtf16) return;
            ShowDetachedWord(selected, (view, point));
        }
        else
        {
            await ShowWordAsync(selected, (view, point));
        }
    }

    private void InlinePdfWordChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(NativeReaderView.SpokenWord)) UpdateInlinePdfHighlight();
    }

    private void UpdateInlinePdfHighlight()
    {
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
        overlay.Drawable = new PdfReadingHighlight(preview.Width, preview.Height, lines, offset,
            sentence.StartUtf16, sentence.EndUtf16, focus?.StartUtf16, focus?.LengthUtf16 ?? 0,
            ReferenceEquals(lines, inlineNativeBoxes));
        overlay.Invalidate();
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
