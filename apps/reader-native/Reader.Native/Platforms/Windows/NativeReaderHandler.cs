using Microsoft.Maui.Handlers;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using WinColor = Windows.UI.Color;

namespace Reader.Native.Controls;

public sealed class NativeReaderHandler : ViewHandler<NativeReaderView, RichEditBox>
{
    private bool updating;
    private Reader.Presentation.WindowsTextMap textMap = new("");
    public static readonly IPropertyMapper<NativeReaderView, NativeReaderHandler> ReaderMapper =
        new PropertyMapper<NativeReaderView, NativeReaderHandler>(ViewMapper)
        {
            [nameof(NativeReaderView.Text)] = MapText,
            [nameof(NativeReaderView.HighlightStart)] = MapText,
            [nameof(NativeReaderView.HighlightLength)] = MapText,
        };
    public NativeReaderHandler() : base(ReaderMapper) { }

    protected override RichEditBox CreatePlatformView() => new()
    {
        IsReadOnly = true, FontSize = 22, TextWrapping = Microsoft.UI.Xaml.TextWrapping.Wrap,
        Background = new SolidColorBrush(WinColor.FromArgb(255, 246, 243, 236)),
        Padding = new Microsoft.UI.Xaml.Thickness(24),
    };

    protected override void ConnectHandler(RichEditBox platformView)
    {
        base.ConnectHandler(platformView);
        platformView.SelectionChanged += Selected;
    }

    protected override void DisconnectHandler(RichEditBox platformView)
    {
        platformView.SelectionChanged -= Selected;
        base.DisconnectHandler(platformView);
    }

    private void Selected(object sender, Microsoft.UI.Xaml.RoutedEventArgs args)
    {
        if (updating) return;
        var range = PlatformView.Document.Selection;
        var start = range.StartPosition;
        var end = range.EndPosition;
        if (start == end)
        {
            var word = PlatformView.Document.GetRange(start, end);
            word.Expand(TextRangeUnit.Word);
            start = word.StartPosition;
            end = word.EndPosition;
        }
        var sourceStart = textMap.SourceOffset(start);
        VirtualView.SelectSource(sourceStart, textMap.SourceOffset(end) - sourceStart);
    }

    private static void MapText(NativeReaderHandler handler, NativeReaderView view)
    {
        handler.updating = true;
        try
        {
            handler.textMap = new Reader.Presentation.WindowsTextMap(view.Text);
            handler.PlatformView.Document.SetText(TextSetOptions.None, handler.textMap.Text);
            var selected = Reader.Presentation.SourceRanges.Selection(view.Text,
                view.HighlightStart, view.HighlightLength);
            if (selected is not null) handler.PlatformView.Document.GetRange(
                handler.textMap.NativeOffset(selected.StartUtf16),
                handler.textMap.NativeOffset(selected.StartUtf16 + selected.LengthUtf16))
                    .CharacterFormat.BackgroundColor =
                    WinColor.FromArgb(255, 222, 230, 213);
        }
        finally { handler.updating = false; }
    }
}
