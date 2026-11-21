using CoreGraphics;
using Foundation;
using PdfKit;
using Reader.Contracts;

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

    internal static Task<PhotoPreview> PreviewAsync(byte[] bytes, int page) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var data = NSData.FromArray(bytes);
        using var document = Open(data);
        return Render(document, page, 1800);
    });

    internal static Task<PdfRecognition> RecognizeAsync(byte[] bytes, string language, IProgress<int> progress) =>
        Task.Run(() =>
        {
            using var pool = new NSAutoreleasePool();
            using var data = NSData.FromArray(bytes);
            using var document = Open(data);
            var count = checked((int)document.PageCount);
            PdfRecognition.CheckPageCount(count);
            var lines = new List<OcrLine>();
            for (var page = 0; page < count; page++)
            {
                progress.Report(page + 1);
                using var pagePool = new NSAutoreleasePool();
                var rendered = Render(document, page, 2600);
                var recognized = PhotoOcr.Recognize(rendered.Bytes, language);
                lines.AddRange(recognized.Select(line => line with { Page = page }));
            }
            return new PdfRecognition(count, lines.ToArray());
        });

    private static PdfDocument Open(NSData data)
    {
        var document = new PdfDocument(data);
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
