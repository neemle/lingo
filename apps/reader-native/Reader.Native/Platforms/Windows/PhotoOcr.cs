using System.Runtime.InteropServices;
using Reader.Contracts;
using Reader.Presentation;
using Windows.Globalization;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;
using OcrEngine = Windows.Media.Ocr.OcrEngine;
using OcrLine = Reader.Contracts.OcrLine;

namespace Reader.Native.Services;

internal static class PhotoOcr
{
    [DllImport("kernel32.dll", ExactSpelling = true)]
    private static extern int GetCurrentPackageFullName(ref uint length, IntPtr buffer);

    internal const string PackageRequired = "Для OCR у Windows потрібна версія Читай, встановлена з пакета MSIX.";

    internal static bool HasPackageIdentity()
    {
        const int noPackage = 15700;
        uint length = 0;
        return GetCurrentPackageFullName(ref length, IntPtr.Zero) != noPackage;
    }

    internal static IReadOnlyList<Language> AvailableRecognitionLanguages() => OcrEngine.AvailableRecognizerLanguages;

    internal static Task<OcrLine[]> RecognizeAsync(byte[] bytes, string language,
        CancellationToken cancellation = default, PhotoPreview? rendered = null) =>
        TesseractOcr.RecognizeAsync(() => RecognizeNativeAsync(bytes, language),
            () => rendered is null ? PreviewAsync(bytes) : Task.FromResult(rendered), language, cancellation);

    private static async Task<OcrLine[]> RecognizeNativeAsync(byte[] bytes, string language)
    {
        if (!HasPackageIdentity()) throw new InvalidOperationException(PackageRequired);

        var selected = AvailableRecognitionLanguages().FirstOrDefault(item =>
            SpeechVoice.MatchesLanguage(item.LanguageTag, language));
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
                null) is not { } line) continue;
            var words = new List<OcrWord>(recognized.Words.Count);
            var cursor = 0;
            foreach (var word in recognized.Words)
            {
                var box = word.BoundingRect;
                var text = word.Text.Trim();
                var offset = PhotoGeometry.MatchWord(line.Text, text, ref cursor);
                if (PhotoGeometry.Word(text, box.X / bitmap.PixelWidth, box.Y / bitmap.PixelHeight,
                    box.Right / bitmap.PixelWidth, box.Bottom / bitmap.PixelHeight, offset) is { } geometry)
                    words.Add(geometry);
            }
            lines.Add(line with { Words = words.ToArray() });
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
