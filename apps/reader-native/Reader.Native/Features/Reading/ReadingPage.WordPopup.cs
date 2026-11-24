using System.ComponentModel;
using Reader.Contracts;
using Reader.Presentation;
using Reader.Native.Controls;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private readonly LocalSpeech wordPopupSpeech = new();
    private CancellationTokenSource? wordDetailRun;
    private (Button Button, string Text, string Description)? wordDetailControl;
    private (VisualElement View, TextSelection Selection, Border Card, PdfWordAnchor? PdfAnchor)? wordPopup;

    private void ShowWordPopup(NativeReaderView view, TextSelection selected, Border card,
        PdfWordAnchor? pdfAnchor = null)
    {
        StopWordDetail();
        CancelWordExplanation();
        CancelSyntaxExplanation();
        WordDetails.IsVisible = DetachedWordDetails.IsVisible = false;
        WordDetailsToggle.Text = DetachedWordDetailsToggle.Text = "Більше";
        wordPopup = (pdfAnchor?.View ?? view, selected, card, pdfAnchor);
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
    }

    private void WordPopupVisibilityChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName != nameof(VisualElement.IsVisible)) return;
        WordOverlay.IsVisible = WordCard.IsVisible || DetachedWordCard.IsVisible;
        if (!WordOverlay.IsVisible)
        {
            StopWordDetail();
            CancelPopupTranslation();
            CancelWordExplanation();
            CancelSyntaxExplanation();
        }
        UpdatePopupTranslationHighlights();
        UpdateInlinePdfHighlight();
    }

    private void WordPopupResized(object? sender, EventArgs args) => PositionWordPopup();

    private void PositionWordPopup()
    {
        if (wordPopup is not { } popup || !popup.Card.IsVisible || ReaderRoot.Width <= 0 || ReaderRoot.Height <= 0)
            return;

        var word = popup.PdfAnchor is { } anchor ? PdfWordBounds(anchor) :
            (popup.View as NativeReaderView)?.SelectionAnchor?.Invoke(popup.Selection.StartUtf16, popup.Selection.LengthUtf16);
        var origin = new Point();
        for (Element? element = popup.View; element is not null && element != ReaderRoot; element = element.Parent)
            if (element is VisualElement visual)
            {
                if (visual is ScrollView scroll)
                {
                    origin = new Point(origin.X - scroll.ScrollX, origin.Y - scroll.ScrollY);
                    if (word is { } bounds)
                        word = ClipWord(bounds, origin, scroll.Width, scroll.Height);
                }
                origin = new Point(origin.X + visual.X, origin.Y + visual.Y);
            }

        var visible = word is { } rect ? ClipWord(rect, origin, ReaderRoot.Width, ReaderRoot.Height) : null;
        var anchorInRoot = visible is { } clipped
            ? new Rect(clipped.X + origin.X, clipped.Y + origin.Y, clipped.Width, clipped.Height) : (Rect?)null;
        var expanded = popup.Card == DetachedWordCard
            ? DetachedWordDetails.IsVisible || DetachedMeanings.IsVisible
            : WordDetails.IsVisible || WordMeanings.IsVisible;
        var placement = FitWordPopup(anchorInRoot, ReaderRoot.Width, ReaderRoot.Height, expanded ? 430 : 278);
        AbsoluteLayout.SetLayoutBounds(popup.Card, placement.Card);
        WordPointer.IsVisible = placement.Pointer is not null;
        if (placement.Pointer is not { } pointer) return;
        WordPointer.Rotation = placement.Rotation;
        AbsoluteLayout.SetLayoutBounds(WordPointer, pointer);
    }

    private Rect? PdfWordBounds(PdfWordAnchor anchor)
    {
        if (inlinePdfAddress != anchor.Address || !showInlineOriginal ||
            inlinePdfPreview is not { Width: > 0, Height: > 0 } preview) return null;
        var fit = OriginalPage.AspectFit(new RectF(0, 0, (float)anchor.View.Width, (float)anchor.View.Height),
            preview.Width, preview.Height);
        if (fit.Width <= 0 || fit.Height <= 0) return null;
        var bounds = anchor.Bounds;
        return new Rect(fit.X + bounds.X * fit.Width, fit.Y + bounds.Y * fit.Height,
            bounds.Width * fit.Width, bounds.Height * fit.Height);
    }

    private static Rect? ClipWord(Rect word, Point origin, double width, double height)
    {
        var left = Math.Max(word.Left, -origin.X);
        var top = Math.Max(word.Top, -origin.Y);
        var right = Math.Min(word.Right, width - origin.X);
        var bottom = Math.Min(word.Bottom, height - origin.Y);
        return right > left && bottom > top ? new Rect(left, top, right - left, bottom - top) : null;
    }

    private static (Rect Card, Rect? Pointer, double Rotation) FitWordPopup(
        Rect? word, double viewportWidth, double viewportHeight, double preferredHeight)
    {
        const double arrow = 8;
        const double clearance = 10;
        var margin = Math.Min(12, Math.Min(viewportWidth, viewportHeight) / 4);
        var width = Math.Min(432, viewportWidth - margin * 2);
        var anchorX = word?.Center.X ?? viewportWidth / 2;
        var wordTop = word?.Top ?? viewportHeight / 4;
        var wordBottom = word?.Bottom ?? wordTop;
        var aboveEdge = Math.Clamp(wordTop - clearance, margin, viewportHeight - margin);
        var belowEdge = Math.Clamp(wordBottom + clearance, margin, viewportHeight - margin);
        var roomAbove = aboveEdge - margin - arrow;
        var roomBelow = viewportHeight - margin - (belowEdge + arrow);
        var above = roomBelow < preferredHeight && roomAbove > roomBelow;
        var available = above ? roomAbove : roomBelow;
        if (word is { } visibleWord && available < Math.Min(preferredHeight, 210))
        {
            var roomLeft = visibleWord.Left - clearance - arrow - margin;
            var roomRight = viewportWidth - margin - (visibleWord.Right + clearance + arrow);
            var onLeft = roomLeft > roomRight;
            var sideWidth = Math.Min(width, Math.Max(roomLeft, roomRight));
            var sideHeight = Math.Min(preferredHeight, viewportHeight - margin * 2);
            if (sideWidth >= 280 && sideHeight > available && sideHeight >= 32)
            {
                var sideLeft = onLeft ? visibleWord.Left - clearance - arrow - sideWidth
                    : visibleWord.Right + clearance + arrow;
                var sideTop = Math.Clamp(visibleWord.Center.Y - sideHeight / 2, margin, viewportHeight - sideHeight - margin);
                var arrowCenterY = Math.Clamp(visibleWord.Center.Y, sideTop + arrow, sideTop + sideHeight - arrow);
                var arrowCenterX = onLeft ? sideLeft + sideWidth + arrow / 2 : sideLeft - arrow / 2;
                return (new Rect(sideLeft, sideTop, sideWidth, sideHeight),
                    new Rect(arrowCenterX - arrow, arrowCenterY - arrow / 2, arrow * 2, arrow), onLeft ? 90 : -90);
            }
        }
        var left = Math.Clamp(anchorX - width / 2, margin, Math.Max(margin, viewportWidth - width - margin));
        if (available <= 0)
            return (new Rect(left, margin, width, Math.Min(preferredHeight, viewportHeight - 2 * margin)), null, 0);
        var height = Math.Min(preferredHeight, available);
        var top = above ? aboveEdge - arrow - height : belowEdge + arrow;
        var card = new Rect(left, top, width, height);
        if (word is null || width < 32) return (card, null, 0);
        var pointer = Math.Clamp(anchorX - arrow, left + arrow, left + width - arrow * 3);
        return (card, new Rect(pointer, above ? top + height : top - arrow, arrow * 2, arrow), above ? 180 : 0);
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
        if (selected is not null && language is not null) await SpeakWordDetailAsync(selected.ReadingText, language, sender as Button);
    }

    private async void SpeakPopupTranslationClicked(object? sender, EventArgs args)
    {
        if (TargetCode is not { } target) return;
        var text = PopupGloss(DetachedWordCard.IsVisible);
        if (text is not null) await SpeakWordDetailAsync(text, target, sender as Button);
    }

    private async void SpeakPopupContextClicked(object? sender, EventArgs args)
    {
        if (DetachedWordCard.IsVisible)
        {
            if (detachedSentence is { Footnote: false } current)
                await SpeakWordDetailAsync(current.SpeechText ?? current.PronunciationText ?? current.NarrationText ?? current.Sentence.Text,
                    current.Language, sender as Button);
        }
        else if (selection is { } selected && AppState.Session.Document is { } document)
        {
            var sentence = Array.Find(document.Sentences, item =>
                item.StartUtf16 <= selected.StartUtf16 && item.EndUtf16 > selected.StartUtf16);
            if (sentence is not null && !IsFootnote(document, sentence))
                await SpeakWordDetailAsync(ReadingPronunciation(document, sentence, EffectiveSpeechText(document, sentence)),
                    document.Language, sender as Button);
        }
    }

    private async void SpeakPopupExplanationClicked(object? sender, EventArgs args)
    {
        var text = DetachedWordCard.IsVisible ? DetachedWordExplanationText.Text : WordExplanationText.Text;
        if (!string.IsNullOrWhiteSpace(text)) await SpeakWordDetailAsync(text, "uk", sender as Button);
    }

    private void StopWordDetail()
    {
        wordDetailRun?.Cancel();
        wordDetailRun = null;
        wordPopupSpeech.Stop();
        RestoreWordDetailControl();
    }

    private void RestoreWordDetailControl()
    {
        if (wordDetailControl is not { } control) return;
        wordDetailControl = null;
        control.Button.Text = control.Text;
        DescribeButton(control.Button, control.Description);
    }

    private async Task SpeakWordDetailAsync(string text, string language, Button? button)
    {
        if (button is not null && wordDetailControl?.Button == button)
        {
            StopWordDetail();
            return;
        }
        if (string.IsNullOrWhiteSpace(text)) return;
        StopSpeech();
        if (speakingDetachedSentence) ClearDetachedSpeech();
        StopWordDetail();
        using var run = new CancellationTokenSource();
        wordDetailRun = run;
        if (button is not null)
        {
            var label = button.Text ?? "Слухати";
            wordDetailControl = (button, label, ToolTipProperties.GetText(button) as string ?? label);
            button.Text = "Зупинити";
            DescribeButton(button, "Зупинити озвучення");
        }
        var token = run.Token;
        var rate = SpeechRatePercent / 100f;

        string? status;
        try
        {
            var voice = await WordDetailVoiceAsync(language, token);
            token.ThrowIfCancellationRequested();
            if (voice?.Engine is { } engine)
            {
                try
                {
                    await modelInference.WaitAsync(token);
                    SpeechAudio clip;
                    try
                    {
                        token.ThrowIfCancellationRequested();
                        clip = await AppState.Session.SynthesizeSpeechTextAsync(text, language, voice.Speaker, rate, engine, token);
                    }
                    finally { modelInference.Release(); }
                    token.ThrowIfCancellationRequested();
                    status = await wordPopupSpeech.PlayWaveAsync(clip, 0, _ => { });
                }
                catch (Exception error) when (!token.IsCancellationRequested) { status = error.Message; }
                token.ThrowIfCancellationRequested();
                if (status is not null && status != LocalSpeech.CompletedStatus)
                {
                    var fallback = await SpeakSystemVoiceAsync(null);
                    status = fallback is null || fallback == LocalSpeech.CompletedStatus ? fallback : $"{status} {fallback}";
                }
            }
            else
            {
                status = await SpeakSystemVoiceAsync(voice?.Id);
            }
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { return; }
        catch (Exception error) { status = error.Message; }
        finally
        {
            if (ReferenceEquals(wordDetailRun, run))
            {
                wordDetailRun = null;
                RestoreWordDetailControl();
            }
        }
        if (!token.IsCancellationRequested && status is not null && status != LocalSpeech.CompletedStatus)
            await (Navigation.ModalStack.LastOrDefault() ?? this).DisplayAlertAsync("Озвучення", status, "Гаразд");

        async Task<string?> SpeakSystemVoiceAsync(string? voiceId)
        {
            token.ThrowIfCancellationRequested();
            var pronounced = await LocalSpeech.PrepareSystemSpeechAsync(AppState.Session, language, text).WaitAsync(token);
            token.ThrowIfCancellationRequested();
            wordPopupSpeech.Rate = rate;
            wordPopupSpeech.PreferredVoice = voiceId == LocalSpeech.SystemDefaultVoice ? null : voiceId;
            return await wordPopupSpeech.SpeakAsync(pronounced, language);
        }
    }

    private Grid WordDetailAudioRow(string text, string language, double fontSize)
    {
        var row = new Grid { ColumnSpacing = 10 };
        row.ColumnDefinitions.Add(new ColumnDefinition(GridLength.Star));
        row.ColumnDefinitions.Add(new ColumnDefinition(GridLength.Auto));
        row.Add(new Label { Text = text, FontSize = fontSize, VerticalOptions = LayoutOptions.Center }, 0);
        var listen = new Button { Text = "Слухати", VerticalOptions = LayoutOptions.Start, Style = (Style)Resources["PlayerButton"] };
        DescribeButton(listen, $"Прослухати · {DocumentLanguages.Name(language)}");
        listen.Clicked += async (_, _) => await SpeakWordDetailAsync(text, language, listen);
        row.Add(listen, 1);
        return row;
    }
}
