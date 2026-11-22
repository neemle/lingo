using Reader.Presentation;

namespace Reader.Native.Controls;

public sealed class NativeReaderView : View
{
    private SourceRanges? sourceRanges;
    public static readonly BindableProperty TextProperty = BindableProperty.Create(
        nameof(Text), typeof(string), typeof(NativeReaderView), "");
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
