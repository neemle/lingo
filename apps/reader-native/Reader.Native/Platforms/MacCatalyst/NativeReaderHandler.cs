using Foundation;
using Microsoft.Maui.Handlers;
using UIKit;

namespace Reader.Native.Controls;

public sealed class NativeReaderHandler : ViewHandler<NativeReaderView, UITextView>
{
    private UITapGestureRecognizer? tap;
    private bool updating;
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
        var preserveSelection = handler.PlatformView.Text == view.Text;
        var selected = handler.PlatformView.SelectedRange;
        using var paragraph = new NSMutableParagraphStyle { LineSpacing = 6 };
        using var text = new NSMutableAttributedString(view.Text, new UIStringAttributes
        {
            Font = UIFont.FromName("Georgia", view.ReadingFontSize) ?? UIFont.SystemFontOfSize(view.ReadingFontSize),
            ForegroundColor = UIColor.FromRGB(72, 75, 64),
            ParagraphStyle = paragraph,
        });
        var selection = Reader.Presentation.SourceRanges.Selection(view.Text,
            view.HighlightStart, view.HighlightLength);
        if (selection is not null) text.AddAttribute(UIStringAttributeKey.BackgroundColor,
            UIColor.FromRGB(245, 240, 229), new NSRange(selection.StartUtf16, selection.LengthUtf16));
        handler.updating = true;
        try
        {
            handler.PlatformView.AttributedText = text;
            handler.spokenWord = null;
            MapSpokenWord(handler, view);
            if (preserveSelection && selected.Location <= view.Text.Length
                && selected.Length <= view.Text.Length - selected.Location)
                handler.PlatformView.SelectedRange = selected;
        }
        finally { handler.updating = false; }
        if (selection is not null)
            handler.PlatformView.ScrollRangeToVisible(new NSRange(selection.StartUtf16, selection.LengthUtf16));
    }
    private static void MapSpokenWord(NativeReaderHandler handler, NativeReaderView view)
    {
        if (handler.PlatformView.Text != view.Text) return;
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
