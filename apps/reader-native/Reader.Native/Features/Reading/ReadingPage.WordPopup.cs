using System.ComponentModel;
using Reader.Presentation;
using Reader.Native.Controls;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private readonly LocalSpeech wordPopupSpeech = new();
    private (VisualElement View, TextSelection Selection, Border Card, Point? PdfPoint)? wordPopup;

    private void ShowWordPopup(NativeReaderView view, TextSelection selected, Border card,
        (VisualElement View, Point Point)? pdfAnchor = null)
    {
        wordPopupSpeech.Stop();
        WordDetails.IsVisible = DetachedWordDetails.IsVisible = false;
        WordDetailsToggle.Text = DetachedWordDetailsToggle.Text = "Більше";
        wordPopup = (pdfAnchor?.View ?? view, selected, card, pdfAnchor?.Point);
        WordCard.IsVisible = card == WordCard;
        DetachedWordCard.IsVisible = card == DetachedWordCard;
        PositionWordPopup();
        _ = (card == DetachedWordCard ? DetachedWordCardScroll : WordCardScroll).ScrollToAsync(0, 0, false);
        UpdateInlinePdfHighlight();
        _ = LoadPopupTranslationAsync(card == DetachedWordCard);
    }

    private void CloseWordPopupClicked(object? sender, EventArgs args)
    {
        WordCard.IsVisible = DetachedWordCard.IsVisible = false;
        wordPopup = null;
        wordPopupSpeech.Stop();
    }

    private void WordPopupVisibilityChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName != nameof(VisualElement.IsVisible)) return;
        WordOverlay.IsVisible = WordCard.IsVisible || DetachedWordCard.IsVisible;
        UpdatePopupTranslationHighlights();
        UpdateInlinePdfHighlight();
    }

    private void WordPopupResized(object? sender, EventArgs args) => PositionWordPopup();

    private void PositionWordPopup()
    {
        if (wordPopup is not { } popup || !popup.Card.IsVisible || ReaderRoot.Width <= 0 || ReaderRoot.Height <= 0)
            return;

        var origin = new Point();
        for (Element? element = popup.View; element is not null && element != ReaderRoot; element = element.Parent)
            if (element is VisualElement visual) origin = new Point(origin.X + visual.X, origin.Y + visual.Y);

        var word = popup.PdfPoint is { } tap ? new Rect(tap.X, tap.Y - 16, 1, 32) :
            (popup.View as NativeReaderView)?.SelectionAnchor?.Invoke(popup.Selection.StartUtf16, popup.Selection.LengthUtf16);
        const double margin = 12;
        const double arrow = 8;
        const double clearance = 10;
        var width = Math.Max(1, Math.Min(432, ReaderRoot.Width - margin * 2));
        var anchorX = word is { } wordBounds ? origin.X + wordBounds.Center.X : ReaderRoot.Width / 2;
        var wordTop = word is { } from ? origin.Y + from.Top : ReaderRoot.Height / 4;
        var wordBottom = word is { } to ? origin.Y + to.Bottom : wordTop;
        var aboveEdge = Math.Clamp(wordTop - clearance, margin, Math.Max(margin, ReaderRoot.Height - margin));
        var belowEdge = Math.Clamp(wordBottom + clearance, margin, Math.Max(margin, ReaderRoot.Height - margin));
        var roomAbove = aboveEdge - margin - arrow;
        var roomBelow = ReaderRoot.Height - margin - (belowEdge + arrow);
        var expanded = popup.Card == DetachedWordCard ? DetachedWordDetails.IsVisible : WordDetails.IsVisible;
        var preferredHeight = expanded ? 430 : 278;
        var above = roomBelow < preferredHeight && roomAbove > roomBelow;
        var height = Math.Max(1, Math.Min(preferredHeight, above ? roomAbove : roomBelow));
        var left = Math.Clamp(anchorX - width / 2, margin, Math.Max(margin, ReaderRoot.Width - width - margin));
        var top = above ? aboveEdge - arrow - height : belowEdge + arrow;

        AbsoluteLayout.SetLayoutBounds(popup.Card, new Rect(left, top, width, height));
        WordPointer.IsVisible = word is not null && width >= 32;
        if (!WordPointer.IsVisible) return;
        var pointer = Math.Clamp(anchorX - arrow, left + arrow, left + width - arrow * 3);
        WordPointer.Rotation = above ? 180 : 0;
        AbsoluteLayout.SetLayoutBounds(WordPointer, new Rect(pointer, above ? top + height : top - arrow, arrow * 2, arrow));
    }

    private void ToggleWordDetailsClicked(object? sender, EventArgs args)
    {
        var details = DetachedWordCard.IsVisible ? DetachedWordDetails : WordDetails;
        details.IsVisible = !details.IsVisible;
        if (sender is Button button) button.Text = details.IsVisible ? "Згорнути" : "Більше";
        PositionWordPopup();
    }

    private async void SpeakPopupWordClicked(object? sender, EventArgs args)
    {
        var selected = DetachedWordCard.IsVisible ? detachedSelection : selection;
        var language = DetachedWordCard.IsVisible ? detachedAddress?.Language : AppState.Session.Document?.Language;
        if (selected is not null && language is not null) await SpeakWordDetailAsync(selected.Text, language);
    }

    private async void SpeakPopupTranslationClicked(object? sender, EventArgs args)
    {
        if (TargetCode is not { } target) return;
        var text = DetachedWordCard.IsVisible ? detachedWordTranslation?.Text : wordTranslation?.Text;
        if (text is not null) await SpeakWordDetailAsync(text, target);
    }

    private async void SpeakPopupContextClicked(object? sender, EventArgs args)
    {
        if (DetachedWordCard.IsVisible)
        {
            if (detachedSentence is { } current) await SpeakWordDetailAsync(current.Sentence.Text, current.Language);
        }
        else if (selection is { } selected && AppState.Session.Document is { } document)
        {
            var sentence = Array.Find(document.Sentences, item =>
                item.StartUtf16 <= selected.StartUtf16 && item.EndUtf16 > selected.StartUtf16);
            if (sentence is not null) await SpeakWordDetailAsync(sentence.Text, document.Language);
        }
    }

    private async void SpeakPopupExplanationClicked(object? sender, EventArgs args)
    {
        var text = DetachedWordCard.IsVisible ? DetachedWordExplanationText.Text : WordExplanationText.Text;
        if (!string.IsNullOrWhiteSpace(text)) await SpeakWordDetailAsync(text, "uk");
    }

    private async Task SpeakWordDetailAsync(string text, string language)
    {
        if (string.IsNullOrWhiteSpace(text)) return;
        StopSpeech();
        if (speakingDetachedSentence) ClearDetachedSpeech();
        wordPopupSpeech.Rate = SpeechRatePercent / 100f;
        var status = await wordPopupSpeech.SpeakAsync(text, language);
        if (status is not null && status != LocalSpeech.CompletedStatus)
        {
            var pages = Navigation.ModalStack;
            await (pages.LastOrDefault() ?? this).DisplayAlertAsync("Озвучення", status, "Гаразд");
        }
    }

    private Grid WordDetailAudioRow(string text, string language, double fontSize)
    {
        var row = new Grid { ColumnSpacing = 10 };
        row.ColumnDefinitions.Add(new ColumnDefinition(GridLength.Star));
        row.ColumnDefinitions.Add(new ColumnDefinition(GridLength.Auto));
        row.Add(new Label { Text = text, FontSize = fontSize, VerticalOptions = LayoutOptions.Center }, 0);
        var listen = new Button { Text = "Слухати", VerticalOptions = LayoutOptions.Start };
        listen.Clicked += async (_, _) => await SpeakWordDetailAsync(text, language);
        row.Add(listen, 1);
        return row;
    }
}
