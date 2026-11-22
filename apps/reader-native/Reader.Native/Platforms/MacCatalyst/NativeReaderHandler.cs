using Foundation;
using Microsoft.Maui.Handlers;
using Reader.Contracts;
using Reader.Presentation;
using UIKit;

namespace Reader.Native.Controls;

public sealed class NativeReaderHandler : ViewHandler<NativeReaderView, UITextView>
{
    private UITapGestureRecognizer? tap;
    private bool updating;
    private string? renderedText;
    private ReaderTextProjection? projection;
    private DocumentBlock[] renderedBlocks = [];
    private bool renderedReflow;
    private int renderedFontSize;
    private Reader.Presentation.TextSelection? highlightedSentence;
    private Reader.Presentation.TextSelection? spokenWord;
    public static readonly IPropertyMapper<NativeReaderView, NativeReaderHandler> ReaderMapper =
        new PropertyMapper<NativeReaderView, NativeReaderHandler>(ViewMapper)
        {
            [nameof(NativeReaderView.Text)] = MapText,
            [nameof(NativeReaderView.Blocks)] = MapText,
            [nameof(NativeReaderView.ReflowPdf)] = MapText,
            [nameof(NativeReaderView.HighlightStart)] = MapText,
            [nameof(NativeReaderView.HighlightLength)] = MapText,
            [nameof(NativeReaderView.SpokenWord)] = MapSpokenWord,
            [nameof(NativeReaderView.ReadingFontSize)] = MapText,
        };
    public NativeReaderHandler() : base(ReaderMapper) { }

    protected override UITextView CreatePlatformView() => new()
    {
        Editable = false, Selectable = true, ScrollEnabled = true,
        BackgroundColor = UIColor.FromRGB(253, 253, 249),
        TextContainerInset = new UIEdgeInsets(14, 16, 16, 16),
        AccessibilityLabel = "Текст документа. Виділіть слово або торкніться його.",
    };

    protected override void ConnectHandler(UITextView platformView)
    {
        base.ConnectHandler(platformView);
        platformView.SelectionChanged += Selected;
        VirtualView.OffsetRevealRequested += RevealOffset;
        VirtualView.SelectionAnchor = SelectionAnchor;
        tap = new UITapGestureRecognizer(Tapped) { CancelsTouchesInView = false };
        tap.ShouldRecognizeSimultaneously = (_, _) => true;
        platformView.AddGestureRecognizer(tap);
    }

    protected override void DisconnectHandler(UITextView platformView)
    {
        platformView.SelectionChanged -= Selected;
        VirtualView.OffsetRevealRequested -= RevealOffset;
        VirtualView.SelectionAnchor = null;
        if (tap is not null) { platformView.RemoveGestureRecognizer(tap); tap.Dispose(); }
        tap = null;
        renderedText = null;
        projection = null;
        renderedBlocks = [];
        highlightedSentence = spokenWord = null;
        base.DisconnectHandler(platformView);
    }

    private void RevealOffset(object? sender, int offset)
    {
        if (renderedText is not { } text || offset < 0 || offset >= text.Length) return;
        var length = char.IsSurrogatePair(text, offset) ? 2 : 1;
        PlatformView.ScrollRangeToVisible(DisplayRange(offset, length));
    }

    private void Selected(object? sender, EventArgs args)
    {
        if (updating) return;
        var range = PlatformView.SelectedRange;
        SelectDisplayedRange((int)range.Location, (int)range.Length);
    }

    private void Tapped()
    {
        if (tap is null) return;
        var position = PlatformView.GetClosestPositionToPoint(tap.LocationInView(PlatformView));
        if (position is null) return;
        var range = PlatformView.Tokenizer.GetRangeEnclosingPosition(position, UITextGranularity.Word,
            UITextDirection.Forward);
        if (range is null) return;
        var start = (int)PlatformView.GetOffsetFromPosition(PlatformView.BeginningOfDocument, range.Start);
        var length = (int)PlatformView.GetOffsetFromPosition(range.Start, range.End);
        SelectDisplayedRange(start, length);
    }

    private void SelectDisplayedRange(int start, int length)
    {
        if (projection?.ToSource(start, length) is { } source)
            VirtualView.SelectSource(source.Start, source.Length);
    }

    private Microsoft.Maui.Graphics.Point? SelectionAnchor(int start, int length)
    {
        var displayed = DisplayRange(start, length);
        var beginning = PlatformView.BeginningOfDocument;
        var first = PlatformView.GetPosition(beginning, displayed.Location);
        var last = PlatformView.GetPosition(beginning, displayed.Location + displayed.Length);
        if (first is null || last is null || PlatformView.GetTextRange(first, last) is not { } range) return null;
        var rect = PlatformView.GetFirstRectForRange(range);
        var bounds = PlatformView.Bounds;
        var x = (double)(rect.X + rect.Width / 2 - bounds.X);
        var y = (double)(rect.Y + rect.Height / 2 - bounds.Y);
        return double.IsFinite(x) && double.IsFinite(y) && x >= 0 && x <= bounds.Width && y >= 0 && y <= bounds.Height
            ? new Microsoft.Maui.Graphics.Point(x, y) : null;
    }

    private NSRange DisplayRange(int start, int length, bool includeLeadingMarker = false)
    {
        var range = projection?.ToDisplay(start, length, includeLeadingMarker) ?? (start, length);
        return new NSRange(range.Item1, range.Item2);
    }

    private static void MapText(NativeReaderHandler handler, NativeReaderView view)
    {
        var source = view.Text;
        var newText = handler.renderedText != source;
        var newFont = handler.renderedFontSize != view.ReadingFontSize;
        var blocks = view.Blocks;
        var reflowChanged = handler.renderedReflow != view.ReflowPdf;
        handler.renderedReflow = view.ReflowPdf;
        var newBlocks = !ReferenceEquals(handler.renderedBlocks, blocks) &&
            !handler.renderedBlocks.AsSpan().SequenceEqual(blocks);
        handler.renderedBlocks = blocks;
        var sentence = view.HighlightedSpan();
        var newLayout = newText || newBlocks || reflowChanged;
        var newSentence = newLayout || sentence != handler.highlightedSentence;
        if (!newSentence && !newFont && !newBlocks) return;

        handler.updating = true;
        try
        {
            if (newLayout || newFont)
            {
                var font = UIFont.FromName("Georgia", view.ReadingFontSize)
                    ?? UIFont.SystemFontOfSize(view.ReadingFontSize);
                if (newLayout)
                {
                    handler.projection = new ReaderTextProjection(source, blocks, view.ReflowPdf);
                    using var paragraph = new NSMutableParagraphStyle { LineSpacing = 6 };
                    using var text = new NSAttributedString(handler.projection.Text, new UIStringAttributes
                    {
                        Font = font,
                        ForegroundColor = UIColor.FromRGB(72, 75, 64),
                        ParagraphStyle = paragraph,
                    });
                    handler.PlatformView.AttributedText = text;
                    handler.renderedText = source;
                }
                if (font is not null)
                    handler.ApplyBlockStyles(source, blocks, font, view.ReadingFontSize, resetBody: !newLayout);
                handler.renderedFontSize = view.ReadingFontSize;
            }

            if (newSentence)
            {
                var storage = handler.PlatformView.TextStorage;
                if (!newLayout && handler.highlightedSentence is { } previous)
                    storage.RemoveAttribute(UIStringAttributeKey.BackgroundColor,
                        handler.DisplayRange(previous.StartUtf16, previous.LengthUtf16));
                if (sentence is not null)
                    storage.AddAttribute(UIStringAttributeKey.BackgroundColor, UIColor.FromRGB(245, 240, 229),
                        handler.DisplayRange(sentence.StartUtf16, sentence.LengthUtf16));
                handler.highlightedSentence = sentence;
                handler.spokenWord = null;
                MapSpokenWord(handler, view);
            }
        }
        finally { handler.updating = false; }
        if (newSentence && sentence is not null)
            handler.PlatformView.ScrollRangeToVisible(handler.DisplayRange(sentence.StartUtf16, sentence.LengthUtf16));
    }

    private void ApplyBlockStyles(string text, DocumentBlock[] blocks, UIFont body, int size, bool resetBody)
    {
        if (projection is not { } layout || layout.Text.Length == 0) return;
        var storage = PlatformView.TextStorage;
        storage.BeginEditing();
        try
        {
            if (resetBody) storage.AddAttribute(UIStringAttributeKey.Font, body, new NSRange(0, layout.Text.Length));
            DocxTableCell? previousCell = null;
            foreach (var block in blocks)
            {
                var start = block.StartUtf16;
                var end = block.EndUtf16;
                if (start < 0 || end <= start || end > text.Length ||
                    char.IsLowSurrogate(text[start]) || end < text.Length && char.IsLowSurrogate(text[end]))
                    continue;

                if (block.Kind == "heading")
                {
                    var headingSize = size + (block.HeadingLevel switch { 1 => 6, 2 => 4, _ => 2 });
                    var heading = UIFont.FromName("Georgia-Bold", headingSize) ?? UIFont.BoldSystemFontOfSize(headingSize);
                    if (heading is not null)
                        storage.AddAttribute(UIStringAttributeKey.Font, heading, DisplayRange(start, end - start));
                }

                if (block.List is not null || block.TableCell is not null)
                {
                    using var style = ParagraphStyle(block, previousCell, size);
                    var paragraphStart = start > 0 ? text.LastIndexOf('\n', start - 1) + 1 : 0;
                    var paragraphEnd = text.IndexOf('\n', end);
                    paragraphEnd = paragraphEnd < 0 ? text.Length : paragraphEnd + 1;
                    storage.AddAttribute(UIStringAttributeKey.ParagraphStyle, style,
                        DisplayRange(paragraphStart, paragraphEnd - paragraphStart, includeLeadingMarker: true));
                }
                previousCell = block.TableCell;
            }
        }
        finally { storage.EndEditing(); }
    }

    private static NSMutableParagraphStyle ParagraphStyle(DocumentBlock block, DocxTableCell? previousCell, int size)
    {
        var list = block.List;
        var cell = block.TableCell;
        var indent = cell is null ? 0 : size / 2 + (int)Math.Min(cell.Column, 4U) * 6;
        if (list is not null) indent += size / 2 + Math.Clamp(list.Level, 0, 8) * 9;
        var hanging = list?.Marker is { Length: > 0 } marker ? Math.Min(marker.Length, 6) * size / 2 : 0;
        var spacing = cell is null ? 0 : previousCell is null || previousCell.Table != cell.Table ? 12
            : previousCell.Row != cell.Row ? 9 : previousCell.Column != cell.Column ? 3 : 0;
        return new NSMutableParagraphStyle
        {
            LineSpacing = 6,
            ParagraphSpacingBefore = spacing,
            FirstLineHeadIndent = indent,
            HeadIndent = indent + hanging,
        };
    }

    private static void MapSpokenWord(NativeReaderHandler handler, NativeReaderView view)
    {
        if (handler.renderedText != view.Text) return;
        var storage = handler.PlatformView.TextStorage;
        if (handler.spokenWord is { } previous)
            storage.AddAttribute(UIStringAttributeKey.BackgroundColor, UIColor.FromRGB(245, 240, 229),
                handler.DisplayRange(previous.StartUtf16, previous.LengthUtf16));

        var word = view.SpokenWord;
        handler.spokenWord = null;
        if (word is not null && word.StartUtf16 >= view.HighlightStart &&
            word.StartUtf16 + word.LengthUtf16 <= view.HighlightStart + view.HighlightLength &&
            word.StartUtf16 + word.LengthUtf16 <= view.Text.Length)
        {
            storage.AddAttribute(UIStringAttributeKey.BackgroundColor, UIColor.FromRGB(247, 201, 112),
                handler.DisplayRange(word.StartUtf16, word.LengthUtf16));
            handler.spokenWord = word;
        }
    }
}
