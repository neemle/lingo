using CoreGraphics;
using Foundation;
using UIKit;
using ImageIO;
using NaturalLanguage;
using Reader.Contracts;
using Reader.Presentation;
using Vision;

namespace Reader.Native.Services;

internal static class PhotoOcr
{
    internal static Task<OcrLine[]> RecognizeAsync(byte[] bytes, string language) =>
        TesseractOcr.RecognizeAsync(() => Task.Run(() => RecognizeNative(bytes, language)),
            () => PreviewAsync(bytes), language, default,
            () => Task.Run(() => RecognizeNative(bytes, language, rotatedOnly: true)));

    internal static OcrLine[] RecognizePdfPage(PhotoPreview rendered, string language, CancellationToken cancellation) =>
        TesseractOcr.RecognizeAsync(() => Task.FromResult(RecognizeNative(rendered.Bytes, language)),
            () => Task.FromResult(rendered), language, cancellation,
            () => Task.FromResult(RecognizeNative(rendered.Bytes, language, rotatedOnly: true))).GetAwaiter().GetResult();

    internal static async Task<PhotoPreview> PreviewAsync(byte[] bytes) =>
        (await ReadingPreviewAsync(bytes, []).ConfigureAwait(false)).Preview;

    internal static Task<(PhotoPreview Preview, Dictionary<int, PdfBoxAppearance> Appearance)> ReadingPreviewAsync(
        byte[] bytes, OcrLine[] lines) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var image = Decode(bytes, 2048);
        using var uiImage = UIImage.FromImage(image);
        using var data = uiImage.AsPNG() ?? throw new InvalidDataException("Не вдалося підготувати перегляд фото.");
        var appearance = new Dictionary<int, PdfBoxAppearance>();
        if (lines.Length > 0) new PdfScan.PreviewColors(image).AddColors(lines, appearance);
        return (new PhotoPreview(data.ToArray(), (int)image.Width, (int)image.Height), appearance);
    });

    internal static Task<string> ReadLanguageSampleAsync(byte[] bytes) =>
        Task.Run(() => ReadLanguageSample(bytes));

    private static string ReadLanguageSample(byte[] bytes)
    {
        using var pool = new NSAutoreleasePool();
        using var image = Decode(bytes, 1800);
        return ReadLanguageSample(image);
    }

    internal static string ReadLanguageSample(CGImage image)
    {
        using var request = CreateRecognitionRequest();
        request.AutomaticallyDetectsLanguage = true;
        using var handler = new VNImageRequestHandler(image, new VNImageOptions());
        if (!handler.Perform([request], out _)) return "";
        return string.Join("\n", (request.Results ?? []).Select(observation =>
            observation.TopCandidates(1).FirstOrDefault()?.String));
    }

    private static CGImage Decode(byte[] bytes, int limit)
    {
        using var data = NSData.FromArray(bytes);
        using var source = CGImageSource.FromData(data)
            ?? throw new InvalidDataException("macOS не може відкрити це зображення.");
        return source.CreateThumbnail(0, new CGImageThumbnailOptions
        {
            CreateThumbnailFromImageAlways = true,
            CreateThumbnailWithTransform = true,
            MaxPixelSize = limit,
        }) ?? throw new InvalidDataException("macOS не може прочитати це зображення.");
    }

    internal static string[] RecognitionLanguageTags()
    {
        using var pool = new NSAutoreleasePool();
        using var request = CreateRecognitionRequest();
        return ReadRecognitionLanguageTags(request);
    }

    private static VNRecognizeTextRequest CreateRecognitionRequest() => new((_, _) => { })
    {
        RecognitionLevel = VNRequestTextRecognitionLevel.Accurate,
        UsesLanguageCorrection = true,
    };

    private static string[] ReadRecognitionLanguageTags(VNRecognizeTextRequest request)
    {
        var languages = request.GetSupportedRecognitionLanguages(out var error);
        if (error is not null) throw new InvalidOperationException(error.LocalizedDescription);
        return languages?.Select(tag => tag.ToString()).ToArray() ?? [];
    }

    private static OcrLine[] RecognizeNative(byte[] bytes, string language, bool rotatedOnly = false)
    {
        using var pool = new NSAutoreleasePool();
        using var image = Decode(bytes, 4096);
        using var request = CreateRecognitionRequest();
        var selected = ReadRecognitionLanguageTags(request).FirstOrDefault(tag =>
            SpeechVoice.MatchesLanguage(tag, language));
        if (selected is null) throw new InvalidOperationException($"OCR мовою «{language}» недоступний у цій macOS.");
        request.RecognitionLanguages = [selected];

        using var handler = new VNImageRequestHandler(image, new VNImageOptions());
        if (!handler.Perform([request], out var error))
            throw new InvalidOperationException(error?.LocalizedDescription ?? "Помилка розпізнавання зображення.");

        var lines = new List<OcrLine>();
        using var tokenizer = new NLTokenizer(NLTokenUnit.Word);
        foreach (var observation in request.Results ?? [])
        {
            if (rotatedOnly && !IsRotatedText(observation, image)) continue;
            var candidate = observation.TopCandidates(1).FirstOrDefault();
            if (candidate is null) continue;
            var bounds = observation.BoundingBox;
            var raw = candidate.String;
            if (PhotoGeometry.Line(raw, bounds.X, 1 - bounds.Y - bounds.Height,
                bounds.X + bounds.Width, 1 - bounds.Y, candidate.Confidence) is not { } line) continue;
            var words = new List<OcrWord>();
            tokenizer.String = raw;
            foreach (var token in tokenizer.GetTokens(new NSRange(0, raw.Length)))
            {
                using var value = token;
                var range = value.RangeValue;
                var start = (int)range.Location;
                var length = (int)range.Length;
                if (start < 0 || start > raw.Length || length <= 0 || length > raw.Length - start) continue;
                var text = raw.Substring(start, length);
                var offset = PhotoGeometry.NormalizedOffset(raw.AsSpan(0, start));
                using var geometry = candidate.GetBoundingBox(range, out _);
                if (geometry is null) continue;
                var word = geometry.BoundingBox;
                if (PhotoGeometry.Word(text, word.X, 1 - word.Y - word.Height,
                    word.X + word.Width, 1 - word.Y, offset) is { } normalized)
                    words.Add(normalized);
            }
            lines.Add(line with { Words = words.ToArray() });
        }
        return lines.ToArray();
    }

    private static bool IsRotatedText(VNRecognizedTextObservation observation, CGImage image)
    {
        // The leading edge of the quad follows the reading direction, even for a single word.
        var dx = ((double)observation.TopRight.X - (double)observation.TopLeft.X) * (double)image.Width;
        var dy = ((double)observation.TopRight.Y - (double)observation.TopLeft.Y) * (double)image.Height;
        return Math.Abs(dy) > Math.Abs(dx) * 1.6 || (dx < 0 && Math.Abs(dx) > Math.Abs(dy) * 1.6);
    }
}
