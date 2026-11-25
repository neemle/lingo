using Reader.Contracts;

namespace Reader.Presentation;

public static class DocumentPages
{
    public static int? SentencePosition(Document document, int page)
    {
        if (TextBounds(document, page) is not { } bounds) return null;
        bool Overlaps(Sentence sentence) => sentence.StartUtf16 < bounds.End && sentence.EndUtf16 > bounds.Start;

        if (document.Sentences.ElementAtOrDefault(document.Position) is { } current && Overlaps(current))
            return document.Position;
        var first = Array.FindIndex(document.Sentences, Overlaps);
        return first >= 0 ? first : null;
    }

    public static (int Start, int End)? TextBounds(Document document, int page)
    {
        if (page < 0 || document.Format != "pdf") return null;

        (int Start, int End)? ValidRange(int start, int end) =>
            start >= 0 && start < end && end <= document.Text.Length ? (start, end) : null;

        if (document.PdfPageStarts is { } starts)
        {
            if (page >= starts.Length || starts[page] is not int start) return null;
            var end = document.Text.Length;
            for (var next = page + 1; next < starts.Length; next++)
            {
                if (starts[next] is not int nextStart) continue;
                end = nextStart;
                break;
            }
            return ValidRange(start, end);
        }

        var blocks = document.Blocks.Where(block => block.Page == page).ToArray();
        if (blocks.Length > 0)
            return ValidRange(blocks[0].StartUtf16, blocks[^1].EndUtf16);

        // Older OCR imports can have line provenance without a block map.
        var spans = document.OcrLines?.Where(line => line.Page == page).Select(line => line.SourceSpan)
            .OfType<OcrSpan>().ToArray();
        return spans is { Length: > 0 }
            ? ValidRange(spans[0].StartUtf16, spans[^1].EndUtf16)
            : null;
    }
}
