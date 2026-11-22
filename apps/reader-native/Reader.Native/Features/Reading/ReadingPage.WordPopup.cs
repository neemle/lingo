using System.ComponentModel;
using Reader.Presentation;
using Reader.Native.Controls;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private (NativeReaderView View, TextSelection Selection, Border Card)? wordPopup;

    private void ShowWordPopup(NativeReaderView view, TextSelection selected, Border card)
    {
        wordPopup = (view, selected, card);
        WordCard.IsVisible = card == WordCard;
        DetachedWordCard.IsVisible = card == DetachedWordCard;
        PositionWordPopup();
    }

    private void CloseWordPopupClicked(object? sender, EventArgs args)
    {
        WordCard.IsVisible = DetachedWordCard.IsVisible = false;
        wordPopup = null;
    }

    private void WordPopupVisibilityChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(VisualElement.IsVisible))
            WordOverlay.IsVisible = WordCard.IsVisible || DetachedWordCard.IsVisible;
    }

    private void WordPopupResized(object? sender, EventArgs args) => PositionWordPopup();

    private void PositionWordPopup()
    {
        if (wordPopup is not { } popup || !popup.Card.IsVisible || ReaderRoot.Width <= 0 || ReaderRoot.Height <= 0)
            return;

        var origin = new Point();
        for (Element? element = popup.View; element is not null && element != ReaderRoot; element = element.Parent)
            if (element is VisualElement visual) origin = new Point(origin.X + visual.X, origin.Y + visual.Y);

        var word = popup.View.SelectionAnchor?.Invoke(popup.Selection.StartUtf16, popup.Selection.LengthUtf16);
        const double margin = 12;
        const double arrow = 8;
        var width = Math.Max(1, Math.Min(430, ReaderRoot.Width - margin * 2));
        var anchor = word is { } point
            ? new Point(origin.X + point.X, origin.Y + point.Y)
            : new Point(ReaderRoot.Width / 2, Math.Min(origin.Y, ReaderRoot.Height / 4));
        var y = Math.Clamp(anchor.Y, margin + arrow, Math.Max(margin + arrow, ReaderRoot.Height - margin - arrow));
        var roomBelow = ReaderRoot.Height - y - margin - arrow;
        var roomAbove = y - margin - arrow;
        var above = roomBelow < 280 && roomAbove > roomBelow;
        var height = Math.Max(1, Math.Min(420, above ? roomAbove : roomBelow));
        var left = Math.Clamp(anchor.X - width / 2, margin, Math.Max(margin, ReaderRoot.Width - width - margin));
        var top = above ? y - arrow - height : y + arrow;

        AbsoluteLayout.SetLayoutBounds(popup.Card, new Rect(left, top, width, height));
        WordPointer.IsVisible = word is not null && width >= 32;
        if (!WordPointer.IsVisible) return;
        var pointer = Math.Clamp(anchor.X - arrow, left + arrow, left + width - arrow * 3);
        WordPointer.Rotation = above ? 180 : 0;
        AbsoluteLayout.SetLayoutBounds(WordPointer, new Rect(pointer, above ? top + height : top - arrow, arrow * 2, arrow));
    }
}
