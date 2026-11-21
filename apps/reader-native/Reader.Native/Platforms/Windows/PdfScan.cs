using Reader.Contracts;
using Windows.Data.Pdf;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace Reader.Native.Services;

internal static class PdfScan
{
    internal static async Task<int> PageCountAsync(byte[] bytes)
    {
        using var stream = await OpenStreamAsync(bytes);
        var document = await OpenAsync(stream);
        return checked((int)document.PageCount);
    }

    internal static async Task<PhotoPreview> PreviewAsync(byte[] bytes, int page)
    {
        using var stream = await OpenStreamAsync(bytes);
        var document = await OpenAsync(stream);
        return await RenderAsync(document, page, 1800);
    }

    internal static async Task<PdfRecognition> RecognizeAsync(byte[] bytes, string language, IProgress<int> progress)
    {
        using var stream = await OpenStreamAsync(bytes);
        var document = await OpenAsync(stream);
        var count = checked((int)document.PageCount);
        PdfRecognition.CheckPageCount(count);
        var lines = new List<OcrLine>();
        for (var page = 0; page < count; page++)
        {
            progress.Report(page + 1);
            var rendered = await RenderAsync(document, page, 2600);
            var recognized = await PhotoOcr.RecognizeAsync(rendered.Bytes, language);
            lines.AddRange(recognized.Select(line => line with { Page = page }));
        }
        return new PdfRecognition(count, lines.ToArray());
    }

    private static async Task<InMemoryRandomAccessStream> OpenStreamAsync(byte[] bytes)
    {
        var stream = new InMemoryRandomAccessStream();
        try
        {
            using var writer = new DataWriter(stream);
            writer.WriteBytes(bytes);
            await writer.StoreAsync();
            writer.DetachStream();
            stream.Seek(0);
            return stream;
        }
        catch
        {
            stream.Dispose();
            throw;
        }
    }

    private static async Task<PdfDocument> OpenAsync(InMemoryRandomAccessStream stream)
    {
        var document = await PdfDocument.LoadFromStreamAsync(stream);
        if (document.IsPasswordProtected || document.PageCount == 0)
            throw new InvalidDataException("PDF пошкоджено або захищено паролем.");
        return document;
    }

    private static async Task<PhotoPreview> RenderAsync(PdfDocument document, int index, int limit)
    {
        using var page = document.GetPage(checked((uint)index));
        var size = page.Size;
        var maxSide = Math.Max(size.Width, size.Height);
        if (!double.IsFinite(maxSide) || maxSide <= 0)
            throw new InvalidDataException("Сторінка PDF має некоректний розмір.");
        var scale = limit / maxSide;
        using var stream = new InMemoryRandomAccessStream();
        var width = (uint)Math.Max(1, Math.Round(size.Width * scale));
        var height = (uint)Math.Max(1, Math.Round(size.Height * scale));
        await page.RenderToStreamAsync(stream, new PdfPageRenderOptions
        {
            DestinationWidth = width,
            DestinationHeight = height,
            BitmapEncoderId = BitmapEncoder.PngEncoderId,
        });
        stream.Seek(0);
        using var reader = new DataReader(stream);
        var length = checked((uint)stream.Size);
        await reader.LoadAsync(length);
        var png = new byte[length];
        reader.ReadBytes(png);
        return new PhotoPreview(png, (int)width, (int)height);
    }
}
