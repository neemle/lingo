using CoreGraphics;
using Foundation;
using Reader.Contracts;
using System.Runtime.InteropServices;
using UIKit;
using Client = Reader.CoreClient.CoreClient;

namespace Reader.Native.Services;

internal static partial class PdfScan
{
    private static PdfBoxAppearance ReadAppearance(NSAttributedString? text, int position, double pageHeight)
    {
        using var attributes = text?.GetAttributes(position, out _);
        var font = attributes?[UIStringAttributeKey.Font] as UIFont;
        var traits = font?.FontDescriptor.SymbolicTraits ?? 0;
        Color? ink = null;
        if (attributes?[UIStringAttributeKey.ForegroundColor] is UIColor color)
        {
            color.GetRGBA(out var red, out var green, out var blue, out var alpha);
            if (alpha > 0) ink = new Color((float)red, (float)green, (float)blue, (float)alpha);
        }
        return new(font?.FamilyName, font is null ? 0 : (double)font.PointSize / pageHeight,
            traits.HasFlag(UIFontDescriptorSymbolicTraits.Bold) ? 700 : 400,
            traits.HasFlag(UIFontDescriptorSymbolicTraits.Italic), ink, Colors.White);
    }

    private sealed class PreviewColors
    {
        private readonly byte[] pixels;
        private readonly int width;
        private readonly int height;

        internal PreviewColors(CGImage image)
        {
            // A reduced bitmap is enough to sample the paper without copying the full preview.
            var scale = Math.Min(1, 1000d / Math.Max(image.Width, image.Height));
            width = Math.Max(1, (int)Math.Round(image.Width * scale));
            height = Math.Max(1, (int)Math.Round(image.Height * scale));
            using var space = CGColorSpace.CreateDeviceRGB();
            using var context = new CGBitmapContext(IntPtr.Zero, width, height, 8, width * 4, space,
                (CGImageAlphaInfo)(CGBitmapFlags.PremultipliedLast | CGBitmapFlags.ByteOrder32Big));
            var bounds = new CGRect(0, 0, width, height);
            context.SetFillColor(1, 1, 1, 1);
            context.FillRect(bounds);
            context.DrawImage(bounds, image);
            pixels = new byte[width * height * 4];
            Marshal.Copy(context.Data, pixels, 0, pixels.Length);
        }

        internal void AddColors(IEnumerable<OcrLine> lines, Dictionary<int, PdfBoxAppearance> appearance)
        {
            var boxes = new Dictionary<int, PdfSampleBox>();
            foreach (var line in lines)
            {
                Add(line.SourceSpan, line.X, line.Y, line.Width, line.Height);
                foreach (var word in line.Words ?? [])
                    Add(word.SourceSpan, word.X, word.Y, word.Width, word.Height);
            }
            if (boxes.Count == 0 || boxes.Count > 32_768) return;
            var sources = boxes.ToArray();
            var colors = Client.SamplePdfColors(width, height, pixels, sources.Select(source => source.Value).ToArray());
            for (var index = 0; index < sources.Length; index++)
            {
                var key = sources[index].Key;
                var sampled = colors[index];
                var background = Rgb(sampled.Background);
                var ink = sampled.Ink is { } measured ? Rgb(measured) : null;
                appearance[key] = appearance.TryGetValue(key, out var style)
                    ? style with { Background = background, Ink = style.Ink ?? ink }
                    : new(null, 0, 400, false, ink, background);
            }

            void Add(OcrSpan? span, double x, double y, double w, double h)
            {
                if (span is { } range)
                    boxes[range.StartUtf16] = new(x, y, w, h);
            }

            static Color Rgb(uint value) =>
                Color.FromRgb((int)(value >> 16), (int)((value >> 8) & 255), (int)(value & 255));
        }
    }
}
