using Reader.Contracts;
using Reader.Presentation;

namespace Reader.Native.Controls;

public sealed class NativeReaderView : View
{
    private SourceRanges? sourceRanges;
    public static readonly BindableProperty TextProperty = BindableProperty.Create(
        nameof(Text), typeof(string), typeof(NativeReaderView), "");
    public static readonly BindableProperty ReflowPdfProperty = BindableProperty.Create(
        nameof(ReflowPdf), typeof(bool), typeof(NativeReaderView), false);
    public bool ReflowPdf { get => (bool)GetValue(ReflowPdfProperty); set => SetValue(ReflowPdfProperty, value); }
    public static readonly BindableProperty BlocksProperty = BindableProperty.Create(
        nameof(Blocks), typeof(DocumentBlock[]), typeof(NativeReaderView));
    public static readonly BindableProperty HighlightStartProperty = BindableProperty.Create(
        nameof(HighlightStart), typeof(int), typeof(NativeReaderView), 0);
    public static readonly BindableProperty HighlightLengthProperty = BindableProperty.Create(
        nameof(HighlightLength), typeof(int), typeof(NativeReaderView), 0);
    public static readonly BindableProperty ReadingFontSizeProperty = BindableProperty.Create(
        nameof(ReadingFontSize), typeof(int), typeof(NativeReaderView), 22);
    public int ReadingFontSize
    {
        get => (int)GetValue(ReadingFontSizeProperty);
        set => SetValue(ReadingFontSizeProperty, value);
    }

    public static readonly BindableProperty SpokenWordProperty = BindableProperty.Create(
        nameof(SpokenWord), typeof(TextSelection), typeof(NativeReaderView));
    public TextSelection? SpokenWord
    {
        get => GetValue(SpokenWordProperty) as TextSelection;
        set => SetValue(SpokenWordProperty, value);
    }
    public string Text { get => GetValue(TextProperty) as string ?? ""; set => SetValue(TextProperty, value); }
    public DocumentBlock[] Blocks { get => GetValue(BlocksProperty) as DocumentBlock[] ?? []; set => SetValue(BlocksProperty, value); }
    public int HighlightStart
    {
        get => (int)GetValue(HighlightStartProperty);
        set => SetValue(HighlightStartProperty, value);
    }
    public int HighlightLength
    {
        get => (int)GetValue(HighlightLengthProperty);
        set => SetValue(HighlightLengthProperty, value);
    }
    public event EventHandler<TextSelection>? SourceSelected;

    internal event EventHandler<int>? OffsetRevealRequested;
    internal Func<int, int, Rect?>? SelectionAnchor { get; set; }

    internal void RevealOffset(int offset) => OffsetRevealRequested?.Invoke(this, offset);

    internal TextSelection? HighlightedSpan() => GetSourceRanges().Selection(HighlightStart, HighlightLength);

    public void SelectSource(int start, int length)
    {
        var ranges = GetSourceRanges();
        var selection = length == 0 ? ranges.WordAt(start) : ranges.Selection(start, length);
        if (selection is not null) SourceSelected?.Invoke(this, selection);
    }

    private SourceRanges GetSourceRanges()
    {
        var text = Text;
        if (sourceRanges?.Text != text) sourceRanges = new SourceRanges(text);
        return sourceRanges;
    }
}
