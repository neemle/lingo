using CoreGraphics;
using Foundation;
using UIKit;
using ImageIO;
using Reader.Contracts;
using Vision;

namespace Reader.Native.Services;

internal static class PhotoOcr
{
    internal static Task<OcrLine[]> RecognizeAsync(byte[] bytes, string language) =>
        Task.Run(() => Recognize(bytes, language));

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

    internal static OcrLine[] Recognize(byte[] bytes, string language)
    {
        using var pool = new NSAutoreleasePool();
        using var image = Decode(bytes, 4096);
        using var request = new VNRecognizeTextRequest((_, _) => { })
        {
            RecognitionLevel = VNRequestTextRecognitionLevel.Accurate,
            UsesLanguageCorrection = true,
        };
        var languages = request.GetSupportedRecognitionLanguages(out var languageError);
        if (languageError is not null) throw new InvalidOperationException(languageError.LocalizedDescription);
        var selected = languages?.Select(tag => tag.ToString()).FirstOrDefault(tag => tag.Equals(language, StringComparison.OrdinalIgnoreCase)
            || tag.StartsWith(language + "-", StringComparison.OrdinalIgnoreCase));
        if (selected is null) throw new InvalidOperationException($"OCR мовою «{language}» недоступний у цій macOS.");
        request.RecognitionLanguages = [selected];

        using var handler = new VNImageRequestHandler(image, new VNImageOptions());
        if (!handler.Perform([request], out var error))
            throw new InvalidOperationException(error?.LocalizedDescription ?? "Помилка розпізнавання зображення.");

        var lines = new List<OcrLine>();
        foreach (var observation in request.Results ?? [])
        {
            var candidate = observation.TopCandidates(1).FirstOrDefault();
            if (candidate is null) continue;
            var bounds = observation.BoundingBox;
            if (PhotoGeometry.Line(candidate.String, bounds.X, 1 - bounds.Y - bounds.Height,
                bounds.X + bounds.Width, 1 - bounds.Y, candidate.Confidence) is { } line)
                lines.Add(line);
        }
        return lines.ToArray();
    }
}
