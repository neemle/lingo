using CoreGraphics;
using Foundation;
using PdfKit;
using Reader.Contracts;
using System.Globalization;
using System.Text;

namespace Reader.Native.Services;

internal static partial class PdfScan
{
    private static ObliqueScanOverlay? FindObliqueScanOverlay(PdfPage page, string text, int offset,
        CancellationToken cancellation)
    {
        if (text.Length is < 8 or > 160 || text.IndexOfAny(['\r', '\n']) >= 0)
            return null;

        var crop = page.GetBoundsForBox(PdfDisplayBox.Crop);
        if (crop.Width <= 0 || crop.Height <= 0) return null;
        var glyphs = StringInfo.ParseCombiningCharacters(text);
        var centers = new List<(double X, double Y, bool StartsWord)>(glyphs.Length);
        var startsWord = true;
        for (var index = 0; index < glyphs.Length; index++)
        {
            var start = glyphs[index];
            if (char.IsWhiteSpace(text, start))
            {
                startsWord = true;
                continue;
            }
            cancellation.ThrowIfCancellationRequested();
            var length = (index + 1 < glyphs.Length ? glyphs[index + 1] : text.Length) - start;
            using var selection = page.GetSelection(new NSRange(offset + start, length));
            if (selection is null || selection.GetBoundsForPage(page) is not { IsEmpty: false } box)
                return null;
            centers.Add(((double)(box.X + box.Width / 2), (double)(box.Y + box.Height / 2), startsWord));
            startsWord = false;
        }
        if (centers.Count < 8) return null;

        var first = centers[0];
        var last = centers[^1];
        var dx = last.X - first.X;
        var dy = last.Y - first.Y;
        if (Math.Abs(dx) < crop.Width * 0.24 || Math.Abs(dy) < crop.Height * 0.24 ||
            !IsDiagonal(dx, dy))
            return null;

        var lengthSquared = dx * dx + dy * dy;
        var distanceTolerance = Math.Min(crop.Width, crop.Height) * 0.025;
        var maximumCrossProduct = distanceTolerance * Math.Sqrt(lengthSquared);
        for (var index = 1; index < centers.Count; index++)
        {
            var previous = centers[index - 1];
            var next = centers[index];
            var stepX = next.X - previous.X;
            var stepY = next.Y - previous.Y;
            if (stepX * dx + stepY * dy <= 0 ||
                (!next.StartsWord && !IsDiagonal(stepX, stepY)) ||
                Math.Abs((next.X - first.X) * dy - (next.Y - first.Y) * dx) > maximumCrossProduct)
                return null;
        }

        // A standalone diagonal native run is an overlay only when the page has a raster image.
        using var graphicsPage = page.Page;
        var dictionary = graphicsPage?.Dictionary;
        for (var depth = 0; dictionary is not null && depth < 16; depth++)
        {
            if (dictionary.GetDictionary("Resources", out var resources) && resources is not null)
            {
                if (!resources.GetDictionary("XObject", out var objects) || objects is null) return null;
                var hasImage = false;
                objects.Apply((_, entry) =>
                {
                    if (!hasImage && entry.TryGetValue(out CGPDFStream? stream) &&
                        stream?.Dictionary is { } image && image.GetName("Subtype", out var type) && type == "Image")
                        hasImage = true;
                });
                if (!hasImage) return null;

                var width = (double)crop.Width;
                var height = (double)crop.Height;
                var rotation = DisplayRotation(page);
                var sideways = rotation is 90 or 270;
                return new(FoldOverlayText(text), DisplayPoint(first.X, first.Y), DisplayPoint(last.X, last.Y),
                    sideways ? height : width, sideways ? width : height);

                (double X, double Y) DisplayPoint(double pageX, double pageY)
                {
                    var x = pageX - (double)crop.X;
                    var y = pageY - (double)crop.Y;
                    return rotation switch
                    {
                        90 => (y, x),
                        180 => (width - x, y),
                        270 => (height - y, width - x),
                        _ => (x, height - y),
                    };
                }
            }
            if (!dictionary.GetDictionary("Parent", out var parent)) break;
            dictionary = parent;
        }
        return null;

        static bool IsDiagonal(double x, double y) =>
            Math.Min(Math.Abs(x), Math.Abs(y)) > 0.60 * Math.Max(Math.Abs(x), Math.Abs(y));
    }

    private static string FoldOverlayText(string text)
    {
        var folded = new StringBuilder(text.Length);
        foreach (var symbol in text.Normalize(NormalizationForm.FormKD).EnumerateRunes())
        {
            if (Rune.IsLetterOrDigit(symbol))
                folded.Append(Rune.ToUpperInvariant(symbol).ToString());
        }
        return folded.ToString();
    }

    private sealed record ObliqueScanOverlay(string FoldedText, (double X, double Y) Start,
        (double X, double Y) End, double Width, double Height)
    {
        internal bool Matches(OcrLine line)
        {
            // Compare the entire recognized line to a fragment of the overlay.
            // When OCR merges body text with an overlay, keep the combined line.
            if (line.Text.Length > 240) return false;
            var text = FoldOverlayText(line.Text);
            if (text.Length < 8 || !FoldedText.Contains(text, StringComparison.Ordinal)) return false;

            var dx = End.X - Start.X;
            var dy = End.Y - Start.Y;
            var length = Math.Sqrt(dx * dx + dy * dy);
            var tolerance = Math.Min(Width, Height) * 0.031;
            if (!NearBaseline(line.X + line.Width / 2, line.Y + line.Height / 2, tolerance))
                return false;
            return line.Words is null || line.Words.All(word =>
                NearBaseline(word.X + word.Width / 2, word.Y + word.Height / 2, tolerance * 1.4));

            bool NearBaseline(double x, double y, double radius)
            {
                var deltaX = x * Width - Start.X;
                var deltaY = y * Height - Start.Y;
                var along = (deltaX * dx + deltaY * dy) / length;
                var across = Math.Abs(deltaX * dy - deltaY * dx) / length;
                return along >= -radius && along <= length + radius && across <= radius;
            }
        }
    }
}
