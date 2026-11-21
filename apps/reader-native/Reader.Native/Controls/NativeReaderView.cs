using Reader.Presentation;

namespace Reader.Native.Controls;

public sealed class NativeReaderView : View
{
    public static readonly BindableProperty TextProperty = BindableProperty.Create(
        nameof(Text), typeof(string), typeof(NativeReaderView), "");
    public static readonly BindableProperty HighlightStartProperty = BindableProperty.Create(
        nameof(HighlightStart), typeof(int), typeof(NativeReaderView), 0);
    public static readonly BindableProperty HighlightLengthProperty = BindableProperty.Create(
        nameof(HighlightLength), typeof(int), typeof(NativeReaderView), 0);
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

    public void SelectSource(int start, int length)
    {
        var selection = length == 0 ? SourceRanges.WordAt(Text, start) : SourceRanges.Selection(Text, start, length);
        if (selection is not null) SourceSelected?.Invoke(this, selection);
    }
}
