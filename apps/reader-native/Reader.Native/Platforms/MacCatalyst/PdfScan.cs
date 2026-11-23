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

    internal static Task<string[]> ExtractTextAsync(string path, CancellationToken cancellation = default) =>
        Task.Run(() => ReadTextLayers(path, allowDeferred: false, cancellation).PageTexts, cancellation);

    internal static Task<(string[] PageTexts, int? DeferredFrom)> ReadTextForIntakeAsync(
        string path, CancellationToken cancellation = default) =>
        Task.Run(() => ReadTextLayers(path, allowDeferred: true, cancellation), cancellation);

    private static (string[] PageTexts, int? DeferredFrom) ReadTextLayers(
        string path, bool allowDeferred, CancellationToken cancellation)
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
        var deferRemainingPages = allowDeferred &&
            (count > maxImmediatePages || new FileInfo(path).Length > Client.MaxImportBytes);
        var pageLimit = deferRemainingPages ? Math.Min(count, initialPages) : count;
        long totalBytes = 0;
        var openingPages = 0;
        for (var index = 0; index < pageLimit; index++)
        {
            cancellation.ThrowIfCancellationRequested();
            using var pagePool = new NSAutoreleasePool();
            using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
            var text = TextLayer(page, out _).Text;
            if (text.Length > 0) totalBytes += Encoding.UTF8.GetByteCount(text) + 2L;
            if (totalBytes > Client.MaxImportBytes)
            {
                if (!allowDeferred) throw new InvalidDataException("Текстовий шар PDF перевищує 20 МіБ.");
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
        int firstOccurrence, string query, int limit, CancellationToken cancellation) =>
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
                var text = TextLayer(page, out _).Text;
                var occurrence = 0;
                foreach (var match in DocumentFind.SourceMatches(text, query))
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
            if (document.PageCount != expectedPages)
                throw new InvalidDataException("Кількість сторінок PDF змінилася.");
            using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
            return TextLayer(page, out _).Text;
        }, cancellation);

    internal static Task<PdfTextOrderEdit[]> ReadStoredTextOrderFixesAsync(string path, int index, string storedText) =>
        Task.Run(() =>
        {
            using var pool = new NSAutoreleasePool();
            using var document = Open(path);
            using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
            if (page.Rotation % 360 != 0) return [];
            var raw = page.Text ?? "";
            var text = raw.Trim();
            var offset = raw.Length - raw.TrimStart().Length;
            PdfOrderedText.WordBounds? Bounds(int start, int length) => TextBounds(page, offset + start, length);
            var accents = PdfOrderedText.Repair(text, Bounds);
            if (text == storedText && accents.Text != text) return accents.Corrections;
            var joined = accents.JoinLigatures(Bounds);
            if (accents.Text == storedText && joined.Text != storedText) return joined.Corrections;
            var width = page.GetBoundsForBox(PdfDisplayBox.Crop).Width;
            var layout = joined.SeparateBlocks(Bounds, width);
            if (joined.Text == storedText) return layout.Corrections;
            if (accents.Text != joined.Text)
            {
                var previousLayout = accents.SeparateBlocks(Bounds, width);
                if (previousLayout.Text == storedText) return previousLayout.JoinLigatures(Bounds).Corrections;
            }
            return PdfOrderedText.MissingBlockBreaks(storedText, layout.Text);
        });

    internal enum LanguageSample { TextLayer, PortableOcr, NativeOcr }

    internal static Task<string[]> ReadLanguageSamplesAsync(
        string path, LanguageSample source, int skip = 0, int count = 5, CancellationToken cancellation = default) => Task.Run(() =>
    {
        cancellation.ThrowIfCancellationRequested();
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        var pageCount = checked((int)document.PageCount);
        PdfRecognition.CheckPageCount(pageCount);
        var lastPage = pageCount - 1;
        int[] probes = [Math.Min(4, lastPage), pageCount / 3, 2 * pageCount / 3, Math.Min(1, lastPage), 0];
        var samples = new List<string>(probes.Length);
        foreach (var index in probes.Distinct().Skip(skip).Take(count))
        {
            cancellation.ThrowIfCancellationRequested();
            using var pagePool = new NSAutoreleasePool();
            var text = "";
            try
            {
                using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
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
                        text = PhotoOcr.ReadLanguageSample(bitmap);
                }
            }
            catch (Exception error) when (error is InvalidDataException or InvalidOperationException)
            {
                System.Diagnostics.Debug.WriteLine(error.Message);
            }
            cancellation.ThrowIfCancellationRequested();
            var length = Math.Min(text.Length, 1600);
            if (length > 0 && char.IsHighSurrogate(text[length - 1])) length--;
            samples.Add(text[..length]);
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
        string path, int index, OcrLine[] recognized, ReadOnlyMemory<char> expectedText = default) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
        using var thumbnail = Thumbnail(page, 1800);
        var image = EncodePreview(thumbnail);
        var bitmap = thumbnail.CGImage is { } pixels ? new PreviewColors(pixels) : null;
        var appearance = new Dictionary<int, PdfBoxAppearance>();
        if (recognized.Length > 0)
        {
            bitmap?.AddColors(recognized, appearance);
            return (image, "", [], appearance);
        }
        var layer = TextLayer(page, out var offset, expectedText);
        var text = layer.Text;
        var crop = page.GetBoundsForBox(PdfDisplayBox.Crop);
        if (text.Length == 0 || crop.Width <= 0 || crop.Height <= 0) return (image, text, [], appearance);
        var boxes = new List<OcrLine>();
        using var attributed = page.AttributedString;
        var attributes = string.Equals(attributed?.Value, page.Text, StringComparison.Ordinal) ? attributed : null;
        var rotation = (page.Rotation % 360 + 360) % 360;
        var pageHeight = (double)(rotation is 90 or 270 ? crop.Width : crop.Height);
        using var inkSpace = CGColorSpace.CreateSrgb();
        // PDFKit emits U+FFFC for artwork. Its selection bounds can span the entire page.
        foreach (System.Text.RegularExpressions.Match word in System.Text.RegularExpressions.Regex.Matches(text, @"[^\s\uFFFC]+"))
        {
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
            boxes.Add(new(word.Value, x, y, width, height, null, index,
                SourceSpan: new(0, 0, word.Index, word.Index + word.Length)));
            appearance[word.Index] = ReadAppearance(attributes, offset + original.Start, pageHeight, inkSpace);
        }
        bitmap?.AddColors(boxes, appearance);
        return (image, text, boxes.ToArray(), appearance);
    });

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

    private static PdfOrderedText TextLayer(PdfPage page, out int offset, ReadOnlyMemory<char> expected = default)
    {
        var raw = page.Text ?? "";
        var text = raw.Trim();
        var nativeOffset = raw.Length - raw.TrimStart().Length;
        offset = nativeOffset;
        // Existing libraries may already be anchored to PDFKit's original order.
        if (page.Rotation % 360 != 0 || expected.Span.StartsWith(text.AsSpan(), StringComparison.Ordinal))
            return new(text);
        PdfOrderedText.WordBounds? Bounds(int start, int length) => TextBounds(page, nativeOffset + start, length);
        var accents = PdfOrderedText.Repair(text, Bounds);
        if (expected.Span.StartsWith(accents.Text.AsSpan(), StringComparison.Ordinal)) return accents;
        var joined = accents.JoinLigatures(Bounds);
        if (expected.Span.StartsWith(joined.Text.AsSpan(), StringComparison.Ordinal)) return joined;
        var width = page.GetBoundsForBox(PdfDisplayBox.Crop).Width;
        var layout = joined.SeparateBlocks(Bounds, width);
        if (expected.IsEmpty || accents.Text == joined.Text ||
            expected.Span.StartsWith(layout.Text.AsSpan(), StringComparison.Ordinal))
            return layout;
        var previousLayout = accents.SeparateBlocks(Bounds, width);
        if (expected.Span.StartsWith(previousLayout.Text.AsSpan(), StringComparison.Ordinal)) return previousLayout;
        var upgraded = previousLayout.JoinLigatures(Bounds);
        return expected.Span.StartsWith(upgraded.Text.AsSpan(), StringComparison.Ordinal) ? upgraded : layout;
    }

    private static PdfOrderedText.WordBounds? TextBounds(PdfPage page, int start, int length)
    {
        using var selection = page.GetSelection(new NSRange(start, length));
        if (selection is null) return null;
        var box = selection.GetBoundsForPage(page);
        if (box.Width <= 0 || box.Height <= 0) return null;
        return new((double)box.Left, (double)box.Right, (double)(box.Y + box.Height / 2), (double)box.Height);
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

    internal static Task RecognizeAsync(string path, string language, int expectedPages, int[] pendingPages,
        Action<int, OcrLine[]> pageCompleted, CancellationToken cancellation) =>
        Task.Run(() =>
        {
            using var pool = new NSAutoreleasePool();
            using var document = Open(path);
            Recognize(document, language, expectedPages, pendingPages, pageCompleted, cancellation);
        }, cancellation);

    private static void Recognize(PdfDocument document, string language, int expectedPages, int[] pendingPages,
        Action<int, OcrLine[]> pageCompleted, CancellationToken cancellation)
    {
        var count = checked((int)document.PageCount);
        PdfRecognition.CheckPageCount(count);
        if (count != expectedPages) throw new InvalidDataException("Кількість сторінок PDF змінилася.");
        foreach (var page in pendingPages)
        {
            cancellation.ThrowIfCancellationRequested();
            using var pagePool = new NSAutoreleasePool();
            var rendered = Render(document, page, 2600);
            cancellation.ThrowIfCancellationRequested();
            var recognized = PhotoOcr.RecognizePdfPage(rendered, language, cancellation);
            pageCompleted(page, recognized.Select(line => line with { Page = page }).ToArray());
        }
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
        return page.GetThumbnail(new CGSize(Math.Max(1, Math.Round((double)bounds.Width * scale)),
            Math.Max(1, Math.Round((double)bounds.Height * scale))), PdfDisplayBox.Crop);
    }
}
