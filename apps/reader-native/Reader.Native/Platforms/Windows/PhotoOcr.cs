using System.Runtime.InteropServices;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;
using OcrEngine = Windows.Media.Ocr.OcrEngine;
using OcrLine = Reader.Contracts.OcrLine;

namespace Reader.Native.Services;

internal static class PhotoOcr
{
    [DllImport("kernel32.dll", ExactSpelling = true)]
    private static extern int GetCurrentPackageFullName(ref uint length, IntPtr buffer);

    internal static async Task<OcrLine[]> RecognizeAsync(byte[] bytes, string language)
    {
        uint length = 0;
        if (GetCurrentPackageFullName(ref length, IntPtr.Zero) == 15700)
            throw new InvalidOperationException("Для OCR у Windows потрібна версія Читай, встановлена з пакета MSIX.");

        var selected = OcrEngine.AvailableRecognizerLanguages.FirstOrDefault(item =>
            item.LanguageTag.Equals(language, StringComparison.OrdinalIgnoreCase) ||
            item.LanguageTag.StartsWith(language + "-", StringComparison.OrdinalIgnoreCase));
        var engine = selected is null ? null : OcrEngine.TryCreateFromLanguage(selected);
        if (engine is null)
            throw new InvalidOperationException($"Додайте розпізнавання тексту для мови «{language}» у Windows.");

        using var bitmap = await DecodeAsync(bytes, Math.Min(OcrEngine.MaxImageDimension, 4096));
        var result = await engine.RecognizeAsync(bitmap);

        var lines = new List<OcrLine>();
        foreach (var recognized in result.Lines)
        {
            if (recognized.Words.Count == 0) continue;
            var boxes = recognized.Words.Select(word => word.BoundingRect).ToArray();
            if (PhotoGeometry.Line(recognized.Text,
                boxes.Min(box => box.X) / bitmap.PixelWidth, boxes.Min(box => box.Y) / bitmap.PixelHeight,
                boxes.Max(box => box.Right) / bitmap.PixelWidth, boxes.Max(box => box.Bottom) / bitmap.PixelHeight,
                null) is { } line)
                lines.Add(line);
        }
        return lines.ToArray();
    }

    internal static async Task<PhotoPreview> PreviewAsync(byte[] bytes)
    {
        using var bitmap = await DecodeAsync(bytes, 2048);
        using var output = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, output);
        encoder.SetSoftwareBitmap(bitmap);
        await encoder.FlushAsync();
        output.Seek(0);
        using var reader = new DataReader(output);
        var size = checked((uint)output.Size);
        await reader.LoadAsync(size);
        var preview = new byte[size];
        reader.ReadBytes(preview);
        return new PhotoPreview(preview, bitmap.PixelWidth, bitmap.PixelHeight);
    }

    private static async Task<SoftwareBitmap> DecodeAsync(byte[] bytes, uint limit)
    {
        using var source = new InMemoryRandomAccessStream();
        using (var writer = new DataWriter(source))
        {
            writer.WriteBytes(bytes);
            await writer.StoreAsync();
            writer.DetachStream();
        }
        source.Seek(0);
        var decoder = await BitmapDecoder.CreateAsync(source);
        var scale = Math.Min(1.0, (double)limit / Math.Max(decoder.PixelWidth, decoder.PixelHeight));
        var transform = new BitmapTransform
        {
            ScaledWidth = (uint)Math.Max(1, Math.Floor(decoder.PixelWidth * scale)),
            ScaledHeight = (uint)Math.Max(1, Math.Floor(decoder.PixelHeight * scale)),
        };
        return await decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore,
            transform, ExifOrientationMode.RespectExifOrientation, ColorManagementMode.DoNotColorManage);
    }
}
