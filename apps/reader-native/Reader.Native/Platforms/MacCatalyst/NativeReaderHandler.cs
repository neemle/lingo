using Foundation;
using Microsoft.Maui.Handlers;
using UIKit;

namespace Reader.Native.Controls;

public sealed class NativeReaderHandler : ViewHandler<NativeReaderView, UITextView>
{
    private UITapGestureRecognizer? tap;
    private bool updating;
    private string? renderedText;
    private int renderedFontSize;
    private Reader.Presentation.TextSelection? highlightedSentence;
    private Reader.Presentation.TextSelection? spokenWord;
    public static readonly IPropertyMapper<NativeReaderView, NativeReaderHandler> ReaderMapper =
        new PropertyMapper<NativeReaderView, NativeReaderHandler>(ViewMapper)
        {
            [nameof(NativeReaderView.Text)] = MapText,
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
        tap = new UITapGestureRecognizer(Tapped) { CancelsTouchesInView = false };
        tap.ShouldRecognizeSimultaneously = (_, _) => true;
        platformView.AddGestureRecognizer(tap);
    }

    protected override void DisconnectHandler(UITextView platformView)
    {
        platformView.SelectionChanged -= Selected;
        if (tap is not null) { platformView.RemoveGestureRecognizer(tap); tap.Dispose(); }
        tap = null;
        renderedText = null;
        highlightedSentence = spokenWord = null;
        base.DisconnectHandler(platformView);
    }

    private void Selected(object? sender, EventArgs args)
    {
        if (updating) return;
        var range = PlatformView.SelectedRange;
        if (range.Length > 0) VirtualView.SelectSource((int)range.Location, (int)range.Length);
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
        VirtualView.SelectSource(start, length);
    }

    private static void MapText(NativeReaderHandler handler, NativeReaderView view)
    {
        var source = view.Text;
        var newText = handler.renderedText != source;
        var newFont = handler.renderedFontSize != view.ReadingFontSize;
        var sentence = view.HighlightedSpan();
        var newSentence = newText || sentence != handler.highlightedSentence;
        if (!newSentence && !newFont) return;

        handler.updating = true;
        try
        {
            if (newText || newFont)
            {
                var font = UIFont.FromName("Georgia", view.ReadingFontSize)
                    ?? UIFont.SystemFontOfSize(view.ReadingFontSize);
                if (newText)
                {
                    using var paragraph = new NSMutableParagraphStyle { LineSpacing = 6 };
                    using var text = new NSAttributedString(source, new UIStringAttributes
                    {
                        Font = font,
                        ForegroundColor = UIColor.FromRGB(72, 75, 64),
                        ParagraphStyle = paragraph,
                    });
                    handler.PlatformView.AttributedText = text;
                    handler.renderedText = source;
                }
                else if (source.Length > 0)
                    handler.PlatformView.TextStorage.AddAttribute(UIStringAttributeKey.Font, font,
                        new NSRange(0, source.Length));
                handler.renderedFontSize = view.ReadingFontSize;
            }

            if (newSentence)
            {
                var storage = handler.PlatformView.TextStorage;
                if (!newText && handler.highlightedSentence is { } previous)
                    storage.RemoveAttribute(UIStringAttributeKey.BackgroundColor,
                        new NSRange(previous.StartUtf16, previous.LengthUtf16));
                if (sentence is not null)
                    storage.AddAttribute(UIStringAttributeKey.BackgroundColor, UIColor.FromRGB(245, 240, 229),
                        new NSRange(sentence.StartUtf16, sentence.LengthUtf16));
                handler.highlightedSentence = sentence;
                handler.spokenWord = null;
                MapSpokenWord(handler, view);
            }
        }
        finally { handler.updating = false; }
        if (newSentence && sentence is not null)
            handler.PlatformView.ScrollRangeToVisible(new NSRange(sentence.StartUtf16, sentence.LengthUtf16));
    }
    private static void MapSpokenWord(NativeReaderHandler handler, NativeReaderView view)
    {
        if (handler.renderedText != view.Text) return;
        var storage = handler.PlatformView.TextStorage;
        if (handler.spokenWord is { } previous)
            storage.AddAttribute(UIStringAttributeKey.BackgroundColor, UIColor.FromRGB(245, 240, 229),
                new NSRange(previous.StartUtf16, previous.LengthUtf16));

        var word = view.SpokenWord;
        handler.spokenWord = null;
        if (word is not null && word.StartUtf16 >= view.HighlightStart &&
            word.StartUtf16 + word.LengthUtf16 <= view.HighlightStart + view.HighlightLength &&
            word.StartUtf16 + word.LengthUtf16 <= view.Text.Length)
        {
            storage.AddAttribute(UIStringAttributeKey.BackgroundColor, UIColor.FromRGB(247, 201, 112),
                new NSRange(word.StartUtf16, word.LengthUtf16));
            handler.spokenWord = word;
        }
    }
}
