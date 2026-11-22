using Microsoft.Maui.Handlers;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml.Controls;
using WinBrush = Microsoft.UI.Xaml.Media.SolidColorBrush;
using WinColor = Windows.UI.Color;

namespace Reader.Native.Controls;

public sealed class NativeReaderHandler : ViewHandler<NativeReaderView, RichEditBox>
{
    private bool updating;
    private Reader.Presentation.TextSelection? highlightedSentence;
    private Reader.Presentation.TextSelection? spokenWord;
    private Reader.Presentation.WindowsTextMap textMap = new("");
    private string sourceText = "";
    private HeadingRange[] headings = [];

    private readonly record struct HeadingRange(int Start, int End, int Level);

    public static readonly IPropertyMapper<NativeReaderView, NativeReaderHandler> ReaderMapper =
        new PropertyMapper<NativeReaderView, NativeReaderHandler>(ViewMapper)
        {
            [nameof(NativeReaderView.Text)] = MapText,
            [nameof(NativeReaderView.Blocks)] = MapBlocks,
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
        sourceText = "";
        textMap = new Reader.Presentation.WindowsTextMap("");
        headings = [];
        highlightedSentence = spokenWord = null;
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
            var newDocument = handler.sourceText != view.Text;
            if (newDocument)
            {
                handler.sourceText = view.Text;
                handler.textMap = new Reader.Presentation.WindowsTextMap(view.Text);
                handler.headings = handler.HeadingRanges(view);
                handler.PlatformView.Document.SetText(TextSetOptions.None, handler.textMap.Text);
                MapFont(handler, view);
                handler.PlatformView.Document.GetRange(0, handler.textMap.Text.Length)
                    .CharacterFormat.BackgroundColor = WinColor.FromArgb(255, 253, 253, 249);
                handler.highlightedSentence = null;
                handler.spokenWord = null;
            }
            var selected = view.HighlightedSpan();
            if (newDocument || selected != handler.highlightedSentence)
            {
                if (handler.highlightedSentence is { } previous)
                    PaintWord(handler, previous, WinColor.FromArgb(255, 253, 253, 249));
                handler.highlightedSentence = selected;
                handler.spokenWord = null;
                if (selected is not null)
                {
                    var range = handler.PlatformView.Document.GetRange(
                        handler.textMap.NativeOffset(selected.StartUtf16),
                        handler.textMap.NativeOffset(selected.StartUtf16 + selected.LengthUtf16));
                    range.CharacterFormat.BackgroundColor = WinColor.FromArgb(255, 245, 240, 229);
                    range.ScrollIntoView(PointOptions.Start | PointOptions.NoHorizontalScroll);
                }
            }
            MapSpokenWord(handler, view);
        }
        finally { handler.updating = false; }
    }

    private HeadingRange[] HeadingRanges(NativeReaderView view) => view.Blocks
        .Where(block => block.Kind == "heading" && block.StartUtf16 >= 0 &&
            block.EndUtf16 > block.StartUtf16 && block.EndUtf16 <= sourceText.Length)
        .Select(block => new HeadingRange(block.StartUtf16, block.EndUtf16, block.HeadingLevel ?? 0))
        .ToArray();

    private static void MapBlocks(NativeReaderHandler handler, NativeReaderView view)
    {
        if (handler.sourceText != view.Text) return;
        var headings = handler.HeadingRanges(view);
        if (handler.headings.AsSpan().SequenceEqual(headings)) return;
        handler.headings = headings;
        MapFont(handler, view);
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
            format.Bold = FormatEffect.Off;
            foreach (var heading in handler.headings)
            {
                var start = handler.textMap.NativeOffset(heading.Start);
                var end = handler.textMap.NativeOffset(heading.End);
                if (end <= start) continue;
                var headingFormat = handler.PlatformView.Document.GetRange(start, end).CharacterFormat;
                headingFormat.Bold = FormatEffect.On;
                var increment = heading.Level switch { 1 => 6, 2 => 4, _ => 2 };
                headingFormat.Size = view.ReadingFontSize + increment;
            }
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
