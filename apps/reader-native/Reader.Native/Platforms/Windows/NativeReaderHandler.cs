using Microsoft.Maui.Handlers;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml.Controls;
using WinBrush = Microsoft.UI.Xaml.Media.SolidColorBrush;
using WinColor = Windows.UI.Color;

namespace Reader.Native.Controls;

public sealed class NativeReaderHandler : ViewHandler<NativeReaderView, RichEditBox>
{
    private bool updating;
    private Reader.Presentation.TextSelection? spokenWord;
    private Reader.Presentation.WindowsTextMap textMap = new("");
    private string sourceText = "";
    public static readonly IPropertyMapper<NativeReaderView, NativeReaderHandler> ReaderMapper =
        new PropertyMapper<NativeReaderView, NativeReaderHandler>(ViewMapper)
        {
            [nameof(NativeReaderView.Text)] = MapText,
            [nameof(NativeReaderView.HighlightStart)] = MapText,
            [nameof(NativeReaderView.HighlightLength)] = MapText,
            [nameof(NativeReaderView.SpokenWord)] = MapSpokenWord,
            [nameof(NativeReaderView.ReadingFontSize)] = MapFont,
        };
    public NativeReaderHandler() : base(ReaderMapper) { }

    protected override RichEditBox CreatePlatformView() => new()
    {
        IsReadOnly = true, FontSize = 22, TextWrapping = Microsoft.UI.Xaml.TextWrapping.Wrap,
        FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Georgia"),
        Foreground = new WinBrush(WinColor.FromArgb(255, 72, 75, 64)),
        Background = new WinBrush(WinColor.FromArgb(255, 253, 253, 249)),
        BorderThickness = new Microsoft.UI.Xaml.Thickness(0),
        Padding = new Microsoft.UI.Xaml.Thickness(16, 14, 16, 16),
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
            if (handler.sourceText != view.Text)
            {
                handler.sourceText = view.Text;
                handler.textMap = new Reader.Presentation.WindowsTextMap(view.Text);
                handler.PlatformView.Document.SetText(TextSetOptions.None, handler.textMap.Text);
                MapFont(handler, view);
            }
            handler.PlatformView.Document.GetRange(0, handler.textMap.Text.Length)
                .CharacterFormat.BackgroundColor = WinColor.FromArgb(255, 253, 253, 249);
            var selected = Reader.Presentation.SourceRanges.Selection(view.Text,
                view.HighlightStart, view.HighlightLength);
            if (selected is not null) handler.PlatformView.Document.GetRange(
                handler.textMap.NativeOffset(selected.StartUtf16),
                handler.textMap.NativeOffset(selected.StartUtf16 + selected.LengthUtf16))
                    .CharacterFormat.BackgroundColor =
                    WinColor.FromArgb(255, 245, 240, 229);
            handler.spokenWord = null;
            MapSpokenWord(handler, view);
        }
        finally { handler.updating = false; }
    }
    private static void MapFont(NativeReaderHandler handler, NativeReaderView view)
    {
        var wasUpdating = handler.updating;
        handler.updating = true;
        try
        {
            handler.PlatformView.FontSize = view.ReadingFontSize;
            if (handler.textMap.Text.Length == 0) return;
            var format = handler.PlatformView.Document.GetRange(0, handler.textMap.Text.Length).CharacterFormat;
            format.Name = "Georgia";
            format.Size = view.ReadingFontSize;
        }
        finally { handler.updating = wasUpdating; }
    }

    private static void MapSpokenWord(NativeReaderHandler handler, NativeReaderView view)
    {
        if (handler.sourceText != view.Text) return;
        var updatingBefore = handler.updating;
        handler.updating = true;
        try
        {
            if (handler.spokenWord is { } previous)
                PaintWord(handler, previous, WinColor.FromArgb(255, 245, 240, 229));
            var word = view.SpokenWord;
            handler.spokenWord = null;
            if (word is not null && word.StartUtf16 >= view.HighlightStart &&
                word.StartUtf16 + word.LengthUtf16 <= view.HighlightStart + view.HighlightLength &&
                word.StartUtf16 + word.LengthUtf16 <= view.Text.Length)
            {
                PaintWord(handler, word, WinColor.FromArgb(255, 247, 201, 112));
                handler.spokenWord = word;
            }
        }
        finally { handler.updating = updatingBefore; }
    }

    private static void PaintWord(NativeReaderHandler handler, Reader.Presentation.TextSelection word, WinColor color) =>
        handler.PlatformView.Document.GetRange(handler.textMap.NativeOffset(word.StartUtf16),
            handler.textMap.NativeOffset(word.StartUtf16 + word.LengthUtf16)).CharacterFormat.BackgroundColor = color;
}
