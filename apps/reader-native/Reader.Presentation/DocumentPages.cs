using Reader.Contracts;

namespace Reader.Presentation;

public static class DocumentPages
{
    public static int? SentencePosition(Document document, int page)
    {
        if (page < 0 || document.Format != "pdf") return null;

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
            return PositionInRange(document, start, end);
        }

        var blocks = document.Blocks.Where(block => block.Page == page).ToArray();
        if (blocks.Length > 0)
            return PositionInRange(document, blocks[0].StartUtf16, blocks[^1].EndUtf16);

        // Older OCR imports can have line provenance without a block map.
        var spans = document.OcrLines?.Where(line => line.Page == page).Select(line => line.SourceSpan)
            .OfType<OcrSpan>().ToArray();
        return spans is { Length: > 0 }
            ? PositionInRange(document, spans[0].StartUtf16, spans[^1].EndUtf16)
            : null;
    }

    private static int? PositionInRange(Document document, int start, int end)
    {
        if (start < 0 || start >= end || end > document.Text.Length) return null;
        bool Overlaps(Sentence sentence) => sentence.StartUtf16 < end && sentence.EndUtf16 > start;

        if (document.Sentences.ElementAtOrDefault(document.Position) is { } current && Overlaps(current))
            return document.Position;
        var first = Array.FindIndex(document.Sentences, Overlaps);
        return first >= 0 ? first : null;
    }
}
