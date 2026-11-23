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
        return JoinDropCaps(lines);
    }

    private static OcrLine[] JoinDropCaps(List<OcrLine> lines)
    {
        var removed = new HashSet<int>();
        for (var index = 0; index < lines.Count; index++)
        {
            var cap = lines[index];
            if (cap.Text.Length != 1 || !char.IsUpper(cap.Text[0])) continue;

            var nearest = -1;
            var best = double.PositiveInfinity;
            for (var next = 0; next < lines.Count; next++)
            {
                var line = lines[next];
                if (line.Text.Length < 3 || !char.IsLetter(line.Text[0]) ||
                    cap.Height < 1.35 * line.Height || cap.Height > 5 * line.Height) continue;
                var right = cap.X + cap.Width;
                var gap = line.X - right;
                if (gap < -0.15 * line.Height || gap > 0.55 * line.Height ||
                    line.Y < cap.Y - 0.20 * line.Height || line.Y > cap.Y + 0.85 * line.Height) continue;
                var distance = Math.Abs(gap) + Math.Abs(line.Y - cap.Y);
                if (distance >= best) continue;
                best = distance;
                nearest = next;
            }
            if (nearest < 0) continue;

            var body = lines[nearest];
            var left = Math.Min(cap.X, body.X);
            var top = Math.Min(cap.Y, body.Y);
            var rightEdge = Math.Max(cap.X + cap.Width, body.X + body.Width);
            var bottom = Math.Max(cap.Y + cap.Height, body.Y + body.Height);
            var words = new List<OcrWord>();
            foreach (var word in body.Words ?? [])
            {
                if (word.RawStartUtf16 == 0)
                {
                    var wordLeft = Math.Min(cap.X, word.X);
                    var wordTop = Math.Min(cap.Y, word.Y);
                    var wordRight = Math.Max(cap.X + cap.Width, word.X + word.Width);
                    var wordBottom = Math.Max(cap.Y + cap.Height, word.Y + word.Height);
                    words.Add(new(cap.Text + word.Text, wordLeft, wordTop, wordRight - wordLeft,
                        wordBottom - wordTop, 0));
                }
                else
                {
                    words.Add(word with { RawStartUtf16 = word.RawStartUtf16 + cap.Text.Length });
                }
            }
            lines[nearest] = body with
            {
                Text = cap.Text + body.Text,
                X = left,
                Y = top,
                Width = rightEdge - left,
                Height = bottom - top,
                Words = words.ToArray(),
            };
            removed.Add(index);
        }
        return lines.Where((_, index) => !removed.Contains(index)).ToArray();
    }
}
