using CoreGraphics;
using Foundation;
using PdfKit;
using Reader.Contracts;
using System.Globalization;
using System.Text;
using System.Text.RegularExpressions;

namespace Reader.Native.Services;

internal static partial class PdfScan
{
    private static List<ObliqueScanOverlay> FindObliqueScanOverlays(PdfPage page, string text, int offset,
        CancellationToken cancellation)
    {
        if (text.Length < 8) return [];
        var crop = page.GetBoundsForBox(PdfDisplayBox.Crop);
        if (crop.Width <= 0 || crop.Height <= 0) return [];
        var width = (double)crop.Width;
        var height = (double)crop.Height;
        var rotation = DisplayRotation(page);
        var sideways = rotation is 90 or 270;
        var overlays = new List<ObliqueScanOverlay>();
        foreach (var line in Regex.EnumerateMatches(text, @"[^\r\n]+"))
        {
            cancellation.ThrowIfCancellationRequested();
            var candidate = text.AsSpan(line.Index, line.Length);
            var leading = candidate.Length - candidate.TrimStart().Length;
            var trimmed = candidate.Trim();
            if (trimmed.Length is < 8 or > 160) continue;
            var body = trimmed.ToString();
            if (ObliqueBaseline(page, body, offset + line.Index + leading, crop, cancellation) is not { } baseline)
                continue;
            // A standalone diagonal native run is an overlay only when the page has a raster image.
            if (overlays.Count == 0 && !HasRasterImage(page)) return [];
            var end = line.Index + line.Length;
            if (end < text.Length && text[end] == '\r') end++;
            if (end < text.Length && text[end] == '\n') end++;
            overlays.Add(new(line.Index, end, FoldOverlayText(body),
                DisplayPoint(baseline.First), DisplayPoint(baseline.Last),
                sideways ? height : width, sideways ? width : height));
        }
        return overlays;

        (double X, double Y) DisplayPoint((double X, double Y) point)
        {
            var x = point.X - (double)crop.X;
            var y = point.Y - (double)crop.Y;
            return rotation switch
            {
                90 => (y, x),
                180 => (width - x, y),
                270 => (height - y, width - x),
                _ => (x, height - y),
            };
        }
    }

    private static ((double X, double Y) First, (double X, double Y) Last)? ObliqueBaseline(
        PdfPage page, string text, int offset, CGRect crop, CancellationToken cancellation)
    {
        var glyphs = StringInfo.ParseCombiningCharacters(text);
        if (glyphs.Length < 8 || Center(0) is not { } first || Center(glyphs.Length - 1) is not { } last)
            return null;
        var dx = last.X - first.X;
        var dy = last.Y - first.Y;
        // Reject upright lines by looking at two glyphs before asking PDFKit for the whole run.
        if (Math.Abs(dx) < crop.Width * 0.24 || Math.Abs(dy) < crop.Height * 0.24 || !IsDiagonal(dx, dy))
            return null;

        var distanceTolerance = Math.Min(crop.Width, crop.Height) * 0.025;
        var maximumCrossProduct = distanceTolerance * Math.Sqrt(dx * dx + dy * dy);
        var previous = first;
        var startsWord = false;
        var count = 1;
        for (var index = 1; index < glyphs.Length; index++)
        {
            if (char.IsWhiteSpace(text, glyphs[index]))
            {
                startsWord = true;
                continue;
            }
            if ((index == glyphs.Length - 1 ? last : Center(index)) is not { } next) return null;
            var stepX = next.X - previous.X;
            var stepY = next.Y - previous.Y;
            if (stepX * dx + stepY * dy <= 0 ||
                (!startsWord && !IsDiagonal(stepX, stepY)) ||
                Math.Abs((next.X - first.X) * dy - (next.Y - first.Y) * dx) > maximumCrossProduct)
                return null;
            count++;
            previous = next;
            startsWord = false;
        }
        return count >= 8 ? (first, last) : null;

        (double X, double Y)? Center(int index)
        {
            cancellation.ThrowIfCancellationRequested();
            var start = glyphs[index];
            var length = (index + 1 < glyphs.Length ? glyphs[index + 1] : text.Length) - start;
            using var selection = page.GetSelection(new NSRange(offset + start, length));
            if (selection is null || selection.GetBoundsForPage(page) is not { IsEmpty: false } box)
                return null;
            return ((double)(box.X + box.Width / 2), (double)(box.Y + box.Height / 2));
        }

        static bool IsDiagonal(double x, double y) =>
            Math.Min(Math.Abs(x), Math.Abs(y)) > 0.60 * Math.Max(Math.Abs(x), Math.Abs(y));
    }

    private static bool HasRasterImage(PdfPage page)
    {
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

    private sealed record ObliqueScanOverlay(int StartUtf16, int EndUtf16, string FoldedText, (double X, double Y) Start,
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
