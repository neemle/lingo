using CoreGraphics;
using Foundation;
using PdfKit;
using Reader.Contracts;
using Reader.Presentation;
using System.Text;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Native.Services;

internal static class PdfScan
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
        // Streamed imports only need the first page and its immediate neighbor.
        var pageLimit = allowDeferred && new FileInfo(path).Length > Client.MaxImportBytes
            ? Math.Min(count, 2) : count;
        long totalBytes = 0;
        var openingPages = 0;
        for (var index = 0; index < pageLimit; index++)
        {
            cancellation.ThrowIfCancellationRequested();
            using var pagePool = new NSAutoreleasePool();
            using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
            var text = page.Text?.Trim() ?? "";
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

    internal static Task<(int Page, string Text)[]> FindUnreadTextAsync(string path, int[] pages,
        string query, int limit, CancellationToken cancellation) =>
        Task.Run(() =>
        {
            using var pool = new NSAutoreleasePool();
            using var document = Open(path);
            var matches = new List<(int Page, string Text)>(limit);
            foreach (var index in pages)
            {
                cancellation.ThrowIfCancellationRequested();
                using var pagePool = new NSAutoreleasePool();
                using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
                var text = page.Text?.Trim() ?? "";
                if (!DocumentFind.Contains(text, query)) continue;
                var excerpt = new System.Globalization.StringInfo(DocumentFind.MatchingExcerpt(text, query));
                matches.Add((index, excerpt.LengthInTextElements > 240
                    ? excerpt.SubstringByTextElements(0, 240) + "…" : excerpt.String));
                if (matches.Count == limit) break;
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
            return page.Text?.Trim() ?? "";
        }, cancellation);

    internal static Task<string[]> ReadLanguageSamplesAsync(string path, bool optical) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        var pageCount = checked((int)document.PageCount);
        PdfRecognition.CheckPageCount(pageCount);
        var lastPage = pageCount - 1;
        int[] probes = [Math.Min(4, lastPage), pageCount / 3, 2 * pageCount / 3, Math.Min(1, lastPage), 0];
        var samples = new List<string>(probes.Length);
        foreach (var index in probes.Distinct())
        {
            using var pagePool = new NSAutoreleasePool();
            var text = "";
            try
            {
                if (optical) text = PhotoOcr.ReadLanguageSample(Render(document, index, 1800).Bytes);
                else
                {
                    using var page = document.GetPage(index);
                    text = page?.Text?.Trim() ?? "";
                }
            }
            catch (Exception error) when (error is InvalidDataException or InvalidOperationException)
            {
                System.Diagnostics.Debug.WriteLine(error.Message);
            }
            var length = Math.Min(text.Length, 1600);
            if (length > 0 && char.IsHighSurrogate(text[length - 1])) length--;
            samples.Add(text[..length]);
        }
        return samples.ToArray();
    });

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

    internal static Task<(PhotoPreview Image, string Text, OcrLine[] Boxes)> ReadingPreviewAsync(
        string path, int index, bool textGeometry) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var document = Open(path);
        var image = Render(document, index, 1800);
        if (!textGeometry) return (image, "", []);
        using var page = document.GetPage(index) ?? throw new InvalidDataException("Сторінку PDF не знайдено.");
        var raw = page.Text ?? "";
        var text = raw.Trim();
        var crop = page.GetBoundsForBox(PdfDisplayBox.Crop);
        if (text.Length == 0 || crop.Width <= 0 || crop.Height <= 0) return (image, text, []);
        var offset = raw.Length - raw.TrimStart().Length;
        var boxes = new List<OcrLine>();
        foreach (System.Text.RegularExpressions.Match word in System.Text.RegularExpressions.Regex.Matches(text, @"\S+"))
        {
            using var selection = page.GetSelection(new NSRange(offset + word.Index, word.Length));
            if (selection is null) continue;
            var box = selection.GetBoundsForPage(page);
            if (box.Width <= 0 || box.Height <= 0) continue;
            var x = (double)((box.X - crop.X) / crop.Width);
            var y = (double)((box.Y - crop.Y) / crop.Height);
            var width = (double)(box.Width / crop.Width);
            var height = (double)(box.Height / crop.Height);
            (x, y, width, height) = ((page.Rotation % 360 + 360) % 360) switch
            {
                90 => (y, x, height, width),
                180 => (1 - x - width, y, width, height),
                270 => (1 - y - height, 1 - x - width, height, width),
                _ => (x, 1 - y - height, width, height),
            };
            boxes.Add(new(word.Value, x, y, width, height, null, index,
                SourceSpan: new(0, 0, word.Index, word.Index + word.Length)));
        }
        return (image, text, boxes.ToArray());
    });

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
        var bounds = page.GetBoundsForBox(PdfDisplayBox.Crop);
        var maxSide = Math.Max((double)bounds.Width, (double)bounds.Height);
        if (!double.IsFinite(maxSide) || maxSide <= 0)
            throw new InvalidDataException("Сторінка PDF має некоректний розмір.");
        var scale = limit / maxSide;
        using var image = page.GetThumbnail(new CGSize(Math.Max(1, Math.Round((double)bounds.Width * scale)),
            Math.Max(1, Math.Round((double)bounds.Height * scale))), PdfDisplayBox.Crop);
        using var png = image.AsPNG() ?? throw new InvalidDataException("Не вдалося відобразити сторінку PDF.");
        var bitmap = image.CGImage;
        return new PhotoPreview(png.ToArray(), bitmap is null ? (int)image.Size.Width : (int)bitmap.Width,
            bitmap is null ? (int)image.Size.Height : (int)bitmap.Height);
    }
}
