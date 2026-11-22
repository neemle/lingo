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
        TesseractOcr.WithFallbackAsync(() => Task.Run(() => RecognizeNative(bytes, language)),
            () => PreviewAsync(bytes), language, default);

    internal static OcrLine[] RecognizePdfPage(PhotoPreview rendered, string language, CancellationToken cancellation) =>
        TesseractOcr.WithFallbackAsync(() => Task.FromResult(RecognizeNative(rendered.Bytes, language)),
            () => Task.FromResult(rendered), language, cancellation).GetAwaiter().GetResult();

    internal static Task<PhotoPreview> PreviewAsync(byte[] bytes) => Task.Run(() =>
    {
        using var pool = new NSAutoreleasePool();
        using var image = Decode(bytes, 2048);
        using var uiImage = UIImage.FromImage(image);
        using var data = uiImage.AsPNG() ?? throw new InvalidDataException("Не вдалося підготувати перегляд фото.");
        return new PhotoPreview(data.ToArray(), (int)image.Width, (int)image.Height);
    });

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

    private static OcrLine[] RecognizeNative(byte[] bytes, string language)
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
}
