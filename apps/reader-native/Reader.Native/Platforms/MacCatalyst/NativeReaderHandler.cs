using Foundation;
using Microsoft.Maui.Handlers;
using UIKit;

namespace Reader.Native.Controls;

public sealed class NativeReaderHandler : ViewHandler<NativeReaderView, UITextView>
{
    private UITapGestureRecognizer? tap;
    public static readonly IPropertyMapper<NativeReaderView, NativeReaderHandler> ReaderMapper =
        new PropertyMapper<NativeReaderView, NativeReaderHandler>(ViewMapper)
        {
            [nameof(NativeReaderView.Text)] = MapText,
            [nameof(NativeReaderView.HighlightStart)] = MapText,
            [nameof(NativeReaderView.HighlightLength)] = MapText,
        };
    public NativeReaderHandler() : base(ReaderMapper) { }

    protected override UITextView CreatePlatformView() => new()
    {
        Editable = false, Selectable = true, ScrollEnabled = true,
        BackgroundColor = UIColor.FromRGB(246, 243, 236),
        TextContainerInset = new UIEdgeInsets(24, 24, 24, 24),
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
        using var text = new NSMutableAttributedString(view.Text, new UIStringAttributes
        {
            Font = UIFont.SystemFontOfSize(22), ForegroundColor = UIColor.FromRGB(45, 49, 43),
        });
        var selection = Reader.Presentation.SourceRanges.Selection(view.Text,
            view.HighlightStart, view.HighlightLength);
        if (selection is not null) text.AddAttribute(UIStringAttributeKey.BackgroundColor,
            UIColor.FromRGB(222, 230, 213), new NSRange(selection.StartUtf16, selection.LengthUtf16));
        handler.PlatformView.AttributedText = text;
        if (selection is not null)
            handler.PlatformView.ScrollRangeToVisible(new NSRange(selection.StartUtf16, selection.LengthUtf16));
    }
}
