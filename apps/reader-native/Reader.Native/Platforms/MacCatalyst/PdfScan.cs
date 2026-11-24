using CoreGraphics;
using Foundation;
using PdfKit;
using Reader.Contracts;
using Reader.Presentation;
using System.Text;
using UIKit;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Native.Services;

internal static partial class PdfScan
{
    internal static Task<int> PageCountAsync(byte[] bytes) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var data = NSData.FromArray(bytes);
        using var document = Open(data);
        return checked((int)document.PageCount);
    });

    internal static Task<int> PageCountAsync(string path) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        return checked((int)document.PageCount);
    });

    internal static Task<(string[] PageTexts, int? DeferredFrom)> ReadTextForIntakeAsync(
        string path, CancellationToken cancellation = default) =>
        Task.Run(() => ReadTextLayers(path, cancellation), cancellation);

    private static (string[] PageTexts, int? DeferredFrom) ReadTextLayers(
        string path, CancellationToken cancellation)
    {
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        var count = checked((int)document.PageCount);
        PdfRecognition.CheckPageCount(count);
        var pages = new string[count];
        const int openingWindowPages = 3;
        const int openingWindowBytes = 1024 * 1024;
        const int maxImmediatePages = 32;
        const int initialPages = 2;
        // Even a small PDF can require expensive layout extraction across many pages.
        var deferRemainingPages = count > maxImmediatePages || new FileInfo(path).Length > Client.MaxImportBytes;
        var pageLimit = deferRemainingPages ? Math.Min(count, initialPages) : count;
        long totalBytes = 0;
        var openingPages = 0;
        for (var index = 0; index < pageLimit; index++)
        {
            cancellation.ThrowIfCancellationRequested();
            using var pagePool = new NSAutoreleasePool();
            using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
            var text = TextLayer(page, out _, cancellation: cancellation).Text;
            if (text.Length > 0) totalBytes += Encoding.UTF8.GetByteCount(text) + 2L;
            if (totalBytes > Client.MaxImportBytes)
            {
                Array.Fill(pages, "", openingPages, count - openingPages);
                return (pages, openingPages);
            }
            pages[index] = text;
            if (index < openingWindowPages && totalBytes <= openingWindowBytes)
                openingPages = index + 1;
        }
        if (pageLimit == count) return (pages, null);
        Array.Fill(pages, "", pageLimit, count - pageLimit);
        return (pages, pageLimit);
    }

    internal static Task<(int Page, int Occurrence, string Text)[]> FindUnreadTextAsync(string path, int[] pages,
        int firstOccurrence, string query, string language, int limit, CancellationToken cancellation) =>
        Task.Run(() =>
        {
            using var pool = new NSAutoreleasePool();
            using var document = Open(path);
            var matches = new List<(int Page, int Occurrence, string Text)>(limit);
            foreach (var index in pages)
            {
                cancellation.ThrowIfCancellationRequested();
                using var pagePool = new NSAutoreleasePool();
                using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
                var text = TextLayer(page, out _, cancellation: cancellation).Text;
                var occurrence = 0;
                foreach (var match in DocumentFind.PdfSourceMatches(text, query, language))
                {
                    cancellation.ThrowIfCancellationRequested();
                    var position = occurrence++;
                    if (index == pages[0] && position < firstOccurrence) continue;
                    var excerpt = new System.Globalization.StringInfo(DocumentFind.MatchingExcerpt(text, match));
                    matches.Add((index, position, excerpt.LengthInTextElements > 240
                        ? excerpt.SubstringByTextElements(0, 240) + "…" : excerpt.String));
                    if (matches.Count == limit) return matches.ToArray();
                }
            }
            return matches.ToArray();
        }, cancellation);

    internal static Task<string> ReadPageTextAsync(string path, int index, int expectedPages, CancellationToken cancellation) =>
        Task.Run(() =>
        {
            cancellation.ThrowIfCancellationRequested();
            using var pool = new NSAutoreleasePool();
            using var document = Open(path);
            CheckRecognitionPageCount(document, expectedPages);
            using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
            var text = TextLayer(page, out _, cancellation: cancellation).Text;
            cancellation.ThrowIfCancellationRequested();
            return text;
        }, cancellation);

    internal static Task<(OcrLine[] Lines, string? TextLayer)> ReadPageForOcrAsync(string path, int index,
        string language, int expectedPages, bool readTextLayer, CancellationToken cancellation) =>
        Task.Run<(OcrLine[], string?)>(() =>
        {
            using var pool = new NSAutoreleasePool();
            using var document = Open(path);
            CheckRecognitionPageCount(document, expectedPages);
            if (readTextLayer)
            {
                using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
                var text = TextLayer(page, out _, cancellation: cancellation).Text;
                cancellation.ThrowIfCancellationRequested();
                if (!string.IsNullOrWhiteSpace(text)) return ([], text);
            }
            return (RecognizePage(document, index, language, cancellation), null);
        }, cancellation);

    internal static Task<PdfTextOrderEdit[]> ReadStoredTextOrderFixesAsync(string path, int index, string storedText) =>
        Task.Run(() =>
        {
            using var pool = new NSAutoreleasePool();
            using var document = Open(path);
            using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
            var raw = page.Text ?? "";
            var text = raw.Trim();
            var offset = raw.Length - raw.TrimStart().Length;
            PdfOrderedText.WordBounds? Bounds(int start, int length) => TextBounds(page, offset + start, length);
            var accents = PdfOrderedText.Repair(text, Bounds);
            if (text == storedText && accents.Text != text) return accents.Corrections;
            var joined = accents.JoinLigatures(Bounds);
            if (accents.Text == storedText && joined.Text != storedText) return joined.Corrections;
            var width = page.GetBoundsForBox(PdfDisplayBox.Crop).Width;
            PdfOrderedText.WordBounds? LayoutBounds(int start, int length) => TextBounds(page, offset + start, length, raw);
            var layout = joined.SeparateBlocks(LayoutBounds, width);
            if (layout.Text == storedText)
            {
                var marked = MarkNativeNoteReferences(page, raw, offset, layout);
                return ReferenceEquals(marked, layout) ? [] : marked.Corrections;
            }
            if (joined.Text == storedText) return layout.Corrections;
            if (accents.Text != joined.Text)
            {
                var previousLayout = accents.SeparateBlocks(LayoutBounds, width);
                if (previousLayout.Text == storedText) return previousLayout.JoinLigatures(Bounds).Corrections;
            }
            return PdfOrderedText.MissingBlockBreaks(storedText, layout.Text);
        });

    internal enum LanguageSample { TextLayer, PortableOcr, NativeOcr }

    internal static Task<string[]> ReadLanguageSamplesAsync(
        string path, LanguageSample source, int[]? sampleSlots = null,
        Func<int, string, Task<bool>>? stopAfterSample = null, CancellationToken cancellation = default) => Task.Run(() =>
    {
        cancellation.ThrowIfCancellationRequested();
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        var pageCount = checked((int)document.PageCount);
        PdfRecognition.CheckPageCount(pageCount);
        var lastPage = pageCount - 1;
        int[] candidates = [Math.Min(4, lastPage), pageCount / 3, 2 * pageCount / 3, Math.Min(1, lastPage), 0];
        var probes = candidates.Distinct().ToArray();
        var samples = new List<string>(probes.Length);
        foreach (var slot in sampleSlots ?? Enumerable.Range(0, probes.Length))
        {
            cancellation.ThrowIfCancellationRequested();
            using var pagePool = new NSAutoreleasePool();
            var text = "";
            try
            {
                using var page = document.GetPage(probes[slot]) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
                if (source == LanguageSample.TextLayer)
                    text = page.Text?.Trim() ?? "";
                else
                {
                    using var image = Thumbnail(page, 1800);
                    if (source == LanguageSample.PortableOcr)
                        text = TesseractOcr.ReadLanguageSampleAsync(
                            () =>
                            {
                                using var encodingPool = new NSAutoreleasePool();
                                return Task.FromResult(EncodePreview(image));
                            }, cancellation).GetAwaiter().GetResult();
                    else if (image.CGImage is { } bitmap)
                        text = PhotoOcr.ReadLanguageSample(bitmap, cancellation);
                }
            }
            catch (Exception error) when (error is InvalidDataException or InvalidOperationException)
            {
                System.Diagnostics.Debug.WriteLine(error.Message);
            }
            cancellation.ThrowIfCancellationRequested();
            var length = Math.Min(text.Length, 1600);
            if (length > 0 && char.IsHighSurrogate(text[length - 1])) length--;
            var sample = text[..length];
            samples.Add(sample);
            if (stopAfterSample?.Invoke(slot, sample).GetAwaiter().GetResult() == true) break;
        }
        return samples.ToArray();
    }, cancellation);

    internal static Task<PhotoPreview> PreviewAsync(string path, int page) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        return Render(document, page, 1800);
    });

    internal static Task<PhotoPreview> PreviewAsync(byte[] bytes, int page) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var data = NSData.FromArray(bytes);
        using var document = Open(data);
        return Render(document, page, 1800);
    });

    internal static Task<(PhotoPreview Image, string Text, OcrLine[] Boxes, Dictionary<int, PdfBoxAppearance> Appearance)> ReadingPreviewAsync(
        string path, int index, OcrLine[] recognized, ReadOnlyMemory<char> expectedText, CancellationToken cancellation) => Task.Run(() =>
    {
        cancellation.ThrowIfCancellationRequested();
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
        using var thumbnail = Thumbnail(page, 1800);
        cancellation.ThrowIfCancellationRequested();
        var image = EncodePreview(thumbnail);
        var appearance = new Dictionary<int, PdfBoxAppearance>();
        if (recognized.Length > 0)
        {
            AddPreviewColors(thumbnail.CGImage, recognized, appearance);
            return (image, "", [], appearance);
        }
        var layer = TextLayer(page, out var offset, expectedText, cancellation);
        var text = layer.Text;
        var crop = page.GetBoundsForBox(PdfDisplayBox.Crop);
        if (text.Length == 0 || crop.Width <= 0 || crop.Height <= 0) return (image, text, [], appearance);
        var boxes = new List<OcrLine>();
        using var attributed = ReadOptionalAttributes(page);
        var raw = page.Text ?? "";
        var attributes = string.Equals(attributed?.Value, raw, StringComparison.Ordinal) ? attributed : null;
        var rotation = DisplayRotation(page);
        var pageHeight = (double)(rotation is 90 or 270 ? crop.Width : crop.Height);
        using var inkSpace = CGColorSpace.CreateSrgb();
        // PDFKit emits U+FFFC for artwork. Its selection bounds can span the entire page.
        foreach (System.Text.RegularExpressions.Match word in System.Text.RegularExpressions.Regex.Matches(text, @"[^\s\uFFFC]+"))
        {
            cancellation.ThrowIfCancellationRequested();
            if (IsPdfListLabel(text, word.Index, word.Length)) continue;
            if (layer.OriginalRange(word.Index, word.Length) is not { } original) continue;
            using var selection = page.GetSelection(new NSRange(offset + original.Start, original.Length));
            if (selection is null) continue;
            var box = selection.GetBoundsForPage(page);
            if (box.Width <= 0 || box.Height <= 0) continue;
            var x = (double)((box.X - crop.X) / crop.Width);
            var y = (double)((box.Y - crop.Y) / crop.Height);
            var width = (double)(box.Width / crop.Width);
            var height = (double)(box.Height / crop.Height);
            (x, y, width, height) = rotation switch
            {
                90 => (y, x, height, width),
                180 => (1 - x - width, y, width, height),
                270 => (1 - y - height, 1 - x - width, height, width),
                _ => (x, 1 - y - height, width, height),
            };
            var direction = TextDirection(page, offset + original.Start, original.Length, raw, box, rotation,
                out var oblique);
            // An axis-aligned word box can cover unrelated artwork under a diagonal label.
            if (oblique) continue;
            boxes.Add(new(word.Value, x, y, width, height, null, index,
                SourceSpan: new(0, 0, word.Index, word.Index + word.Length), Rotation: direction));
            if (attributes is not null)
            {
                if (ReadAppearance(attributes, offset + original.Start, pageHeight, inkSpace) is { } style)
                    appearance[word.Index] = style;
                else
                    attributes = null;
            }
        }
        cancellation.ThrowIfCancellationRequested();
        AddPreviewColors(thumbnail.CGImage, boxes, appearance);
        return (image, text, boxes.ToArray(), appearance);
    }, cancellation);

    private static int DisplayRotation(PdfPage page) => (int)((page.Rotation % 360 + 360) % 360);

    private static int? TextDirection(PdfPage page, int start, int length, string raw, CGRect word, int pageRotation,
        out bool oblique)
    {
        oblique = false;
        var source = raw.AsSpan(start, length);
        var split = System.Globalization.StringInfo.GetNextTextElementLength(source);
        if (split == length) return null;

        // PDFKit character bounds follow the individual text matrix. Selections use page coordinates.
        using var leading = page.GetSelection(new NSRange(start, split));
        if (leading is null) return null;
        var first = leading.GetBoundsForPage(page);
        if (first.IsEmpty) return null;

        var dx = (double)(word.X - first.X + (word.Width - first.Width) / 2);
        var dy = (double)(word.Y - first.Y + (word.Height - first.Height) / 2);
        int direction;
        if (Math.Abs(dx) > Math.Abs(dy) * 1.6) direction = dx > 0 ? 0 : 180;
        else if (Math.Abs(dy) > Math.Abs(dx) * 1.6) direction = dy < 0 ? 90 : 270;
        else
        {
            oblique = Math.Max(Math.Abs(dx), Math.Abs(dy)) > Math.Min(word.Width, word.Height) * 0.12;
            return null;
        }
        return (direction + pageRotation) % 360;
    }

    private static bool IsPdfListLabel(string text, int start, int length)
    {
        var token = text.AsSpan(start, length);
        var end = start + length;
        if (token.Length is < 2 or > 4 || token[^1] != '.' ||
            end == text.Length || !char.IsWhiteSpace(text[end]) || text[end] is '\r' or '\n')
            return false;
        if (!(token.Length == 2 && token[0] is >= 'a' and <= 'z') &&
            token[..^1].IndexOfAnyExceptInRange('0', '9') >= 0) return false;
        for (var i = start - 1; i >= 0 && text[i] is not ('\r' or '\n'); i--)
            if (!char.IsWhiteSpace(text[i]) && text[i] != '\uFFFC') return false;
        return true;
    }

    private static PdfOrderedText TextLayer(PdfPage page, out int offset, ReadOnlyMemory<char> expected = default,
        CancellationToken cancellation = default)
    {
        cancellation.ThrowIfCancellationRequested();
        var raw = page.Text ?? "";
        var text = raw.Trim();
        var nativeOffset = raw.Length - raw.TrimStart().Length;
        offset = nativeOffset;
        if (expected.IsEmpty && IsObliqueScanOverlay(page, text, nativeOffset, cancellation))
            return new("");
        // Existing libraries may already be anchored to PDFKit's original order.
        if (expected.Span.StartsWith(text.AsSpan(), StringComparison.Ordinal))
            return new(text);
        PdfOrderedText.WordBounds? Bounds(int start, int length)
        {
            cancellation.ThrowIfCancellationRequested();
            return TextBounds(page, nativeOffset + start, length);
        }
        var accents = PdfOrderedText.Repair(text, Bounds);
        if (expected.Span.StartsWith(accents.Text.AsSpan(), StringComparison.Ordinal)) return accents;
        var joined = accents.JoinLigatures(Bounds);
        if (expected.Span.StartsWith(joined.Text.AsSpan(), StringComparison.Ordinal)) return joined;
        var width = page.GetBoundsForBox(PdfDisplayBox.Crop).Width;
        PdfOrderedText.WordBounds? LayoutBounds(int start, int length)
        {
            cancellation.ThrowIfCancellationRequested();
            return TextBounds(page, nativeOffset + start, length, raw);
        }
        var layout = joined.SeparateBlocks(LayoutBounds, width);
        if (expected.Span.StartsWith(layout.Text.AsSpan(), StringComparison.Ordinal)) return layout;
        var marked = MarkNativeNoteReferences(page, raw, nativeOffset, layout, cancellation);
        if (expected.IsEmpty || accents.Text == joined.Text ||
            expected.Span.StartsWith(marked.Text.AsSpan(), StringComparison.Ordinal))
            return marked;
        var previousLayout = accents.SeparateBlocks(LayoutBounds, width);
        if (expected.Span.StartsWith(previousLayout.Text.AsSpan(), StringComparison.Ordinal)) return previousLayout;
        var upgraded = previousLayout.JoinLigatures(Bounds);
        return expected.Span.StartsWith(upgraded.Text.AsSpan(), StringComparison.Ordinal) ? upgraded : marked;
    }

    private static PdfOrderedText MarkNativeNoteReferences(PdfPage page, string raw, int offset, PdfOrderedText layout,
        CancellationToken cancellation = default)
    {
        NSAttributedString? attributes = null;
        var loaded = false;
        try
        {
            return layout.MarkRaisedNoteReferences((start, length) =>
            {
                cancellation.ThrowIfCancellationRequested();
                if (!loaded)
                {
                    loaded = true;
                    attributes = page.AttributedString;
                    if (attributes is not null && !string.Equals(attributes.Value, raw, StringComparison.Ordinal))
                    {
                        attributes.Dispose();
                        attributes = null;
                    }
                }
                if (attributes is null) return false;
                var position = offset + start;
                var bodySize = PointSize(attributes, position - 1);
                if (bodySize <= 0) return false;
                for (var index = position; index < position + length; index++)
                {
                    var size = PointSize(attributes, index);
                    if (size <= 0 || size > bodySize * 0.75) return false;
                }
                return true;
            });
        }
        finally { attributes?.Dispose(); }

        static double PointSize(NSAttributedString source, int position)
        {
            using var values = source.GetAttributes(position, out _);
            return values?[UIStringAttributeKey.Font] is UIFont font ? (double)font.PointSize : 0;
        }
    }

    private static PdfOrderedText.WordBounds? TextBounds(PdfPage page, int start, int length, string? raw = null)
    {
        using var selection = page.GetSelection(new NSRange(start, length));
        if (selection is null) return null;
        var box = selection.GetBoundsForPage(page);
        if (box.Width <= 0 || box.Height <= 0) return null;
        var rotation = raw is null ? null : TextDirection(page, start, length, raw, box, pageRotation: 0, out _);
        return new((double)box.Left, (double)box.Right, (double)(box.Y + box.Height / 2), (double)box.Height, rotation);
    }

    internal static Task RecognizeAsync(byte[] bytes, string language, int expectedPages, int[] pendingPages,
        Action<int, OcrLine[]> pageCompleted, CancellationToken cancellation) =>
        Task.Run(() =>
        {
            using var pool = new NSAutoreleasePool();
            using var data = NSData.FromArray(bytes);
            using var document = Open(data);
            Recognize(document, language, expectedPages, pendingPages, pageCompleted, cancellation);
        }, cancellation);

    private static void Recognize(PdfDocument document, string language, int expectedPages, int[] pendingPages,
        Action<int, OcrLine[]> pageCompleted, CancellationToken cancellation)
    {
        CheckRecognitionPageCount(document, expectedPages);
        foreach (var page in pendingPages)
            pageCompleted(page, RecognizePage(document, page, language, cancellation));
    }

    private static OcrLine[] RecognizePage(PdfDocument document, int page, string language, CancellationToken cancellation)
    {
        cancellation.ThrowIfCancellationRequested();
        using var pool = new NSAutoreleasePool();
        var rendered = Render(document, page, 2600);
        cancellation.ThrowIfCancellationRequested();
        var recognized = PhotoOcr.RecognizePdfPage(rendered, language, cancellation);
        cancellation.ThrowIfCancellationRequested();
        return recognized.Select(line => line with { Page = page }).ToArray();
    }

    private static void CheckRecognitionPageCount(PdfDocument document, int expectedPages)
    {
        var count = checked((int)document.PageCount);
        PdfRecognition.CheckPageCount(count);
        if (count != expectedPages) throw new InvalidDataException("Кількість сторінок PDF змінилася.");
    }

    private static PdfDocument Open(NSData data) => Checked(new PdfDocument(data));

    private static PdfDocument Open(string path)
    {
        using var url = NSUrl.FromFilename(path);
        return Checked(new PdfDocument(url));
    }

    private static PdfDocument Checked(PdfDocument document)
    {
        if (!document.IsLocked && document.PageCount > 0) return document;
        document.Dispose();
        throw new InvalidDataException("PDF пошкоджено або захищено паролем.");
    }

    private static PhotoPreview Render(PdfDocument document, int index, int limit)
    {
        using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
        using var image = Thumbnail(page, limit);
        return EncodePreview(image);
    }

    private static PhotoPreview EncodePreview(UIImage image)
    {
        using var png = image.AsPNG() ?? throw new InvalidDataException("Не вдалося відобразити сторінку PDF.");
        var bitmap = image.CGImage;
        return new PhotoPreview(png.ToArray(), bitmap is null ? (int)image.Size.Width : (int)bitmap.Width,
            bitmap is null ? (int)image.Size.Height : (int)bitmap.Height);
    }

    private static UIImage Thumbnail(PdfPage page, int limit)
    {
        var bounds = page.GetBoundsForBox(PdfDisplayBox.Crop);
        var maxSide = Math.Max((double)bounds.Width, (double)bounds.Height);
        if (!double.IsFinite(maxSide) || maxSide <= 0)
            throw new InvalidDataException("Сторінка PDF має некоректний розмір.");
        var scale = limit / maxSide;
        var sideways = DisplayRotation(page) is 90 or 270;
        var width = sideways ? bounds.Height : bounds.Width;
        var height = sideways ? bounds.Width : bounds.Height;
        return page.GetThumbnail(new CGSize(Math.Max(1, Math.Round((double)width * scale)),
            Math.Max(1, Math.Round((double)height * scale))), PdfDisplayBox.Crop);
    }
}
