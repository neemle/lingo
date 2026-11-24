using CoreGraphics;
using Foundation;
using PdfKit;
using System.Globalization;

namespace Reader.Native.Services;

internal static partial class PdfScan
{
    private static bool IsObliqueScanOverlay(PdfPage page, string text, int offset, CancellationToken cancellation)
    {
        if (text.Length is < 8 or > 160 || text.IndexOfAny(['\r', '\n']) >= 0)
            return false;

        var crop = page.GetBoundsForBox(PdfDisplayBox.Crop);
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
                return false;
            centers.Add(((double)(box.X + box.Width / 2), (double)(box.Y + box.Height / 2), startsWord));
            startsWord = false;
        }
        if (centers.Count < 8) return false;

        var first = centers[0];
        var last = centers[^1];
        var dx = last.X - first.X;
        var dy = last.Y - first.Y;
        if (Math.Abs(dx) < crop.Width * 0.24 || Math.Abs(dy) < crop.Height * 0.24 ||
            !IsDiagonal(dx, dy))
            return false;

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
                return false;
        }

        // The only embedded text is a long oblique run over an image. Let the
        // existing per-page OCR path read the raster instead.
        using var graphicsPage = page.Page;
        var dictionary = graphicsPage?.Dictionary;
        for (var depth = 0; dictionary is not null && depth < 16; depth++)
        {
            if (dictionary.GetDictionary("Resources", out var resources) && resources is not null)
            {
                if (!resources.GetDictionary("XObject", out var objects) || objects is null) return false;
                var hasImage = false;
                objects.Apply((_, entry) =>
                {
                    if (!hasImage && entry.TryGetValue(out CGPDFStream? stream) &&
                        stream?.Dictionary is { } image && image.GetName("Subtype", out var type) && type == "Image")
                        hasImage = true;
                });
                return hasImage;
            }
            if (!dictionary.GetDictionary("Parent", out var parent)) break;
            dictionary = parent;
        }
        return false;

        static bool IsDiagonal(double x, double y) =>
            Math.Min(Math.Abs(x), Math.Abs(y)) > 0.60 * Math.Max(Math.Abs(x), Math.Abs(y));
    }
}
