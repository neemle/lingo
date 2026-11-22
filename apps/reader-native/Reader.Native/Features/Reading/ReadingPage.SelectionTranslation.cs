using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private int popupTranslationRequest;
    private (string Document, string Revision, TextSelection Selection, string Target, string Text, string Source)? wordTranslation;
    private (string Target, string Text, string Source)? detachedWordTranslation;

    private async void TranslateSelectionClicked(object? sender, EventArgs args) =>
        await LoadPopupTranslationAsync(detached: false);

    private async void TranslateDetachedSelectionClicked(object? sender, EventArgs args) =>
        await LoadPopupTranslationAsync(detached: true);

    private async Task LoadPopupTranslationAsync(bool detached)
    {
        var request = ++popupTranslationRequest;
        var session = AppState.Session;
        var label = detached ? DetachedWordTranslationGloss : WordTranslationGloss;
        var card = detached ? DetachedWordCard : WordCard;
        var selected = detached ? detachedSelection : selection;
        var document = session.Document;
        var address = detachedAddress;
        var revision = detached ? detachedSentence?.RevisionId : document?.CurrentRevisionId;
        var target = TargetCode;
        var source = detached ? address?.Language : document?.Language;
        var listen = detached ? DetachedSpeakWordTranslation : SpeakWordTranslation;
        listen.IsEnabled = false;
        label.Text = "Виберіть мову перекладу.";
        if (!detached) wordTranslation = null;
        else detachedWordTranslation = null;
        UpdatePopupTranslationHighlights();
        if (selected is null || document is null || revision is null || target is null || target == source) return;

        bool Current() => request == popupTranslationRequest && readingPageActive && card.IsVisible &&
            TargetCode == target && session.Document?.Id == document.Id &&
            (detached
                ? ReferenceEquals(detachedAddress, address) && detachedSelection == selected && detachedSentence?.RevisionId == revision
                : selection == selected && session.Document?.CurrentRevisionId == revision);

        label.Text = "Перекладаємо виділене…";
        try
        {
            await Task.Delay(120);
            if (!Current()) return;
            var result = await session.TranslateSelectionAsync(document.Id, target, revision, selected,
                detached ? address?.Page : null);
            if (!Current()) return;
            if (result is null)
            {
                label.Text = "Для перекладу потрібні мовні пакети.";
                return;
            }

            var original = result.SourceText ?? selected.Text.Trim();
            label.Text = result.Text;
            listen.IsEnabled = !string.IsNullOrWhiteSpace(result.Text);
            if (detached) detachedWordTranslation = (target, result.Text, original);
            else
            {
                wordTranslation = (document.Id, revision, selected, target, result.Text, original);
                if (string.IsNullOrWhiteSpace(Translation.Text)) Translation.Text = result.Text;
            }
            UpdatePopupTranslationHighlights();
        }
        catch (Exception error)
        {
            if (Current()) label.Text = error.Message;
        }
    }

    private void PopupTranslationTextChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(Label.Text)) UpdatePopupTranslationHighlights();
    }

    private void UpdatePopupTranslationHighlights()
    {
        var target = TargetCode;
        var canonicalGloss = WordCard.IsVisible && wordTranslation is { } word && word.Target == target ? word.Text : null;
        var detachedGloss = DetachedWordCard.IsVisible && detachedWordTranslation is { } detached && detached.Target == target
            ? detached.Text : null;
        HighlightTranslation(SegmentTranslation, SegmentTranslationHighlighted, canonicalGloss);
        HighlightTranslation(ModelTranslationText, ModelTranslationTextHighlighted, canonicalGloss);
        HighlightTranslation(DetachedTranslation, DetachedTranslationHighlighted, detachedGloss);
        HighlightTranslation(DetachedModelTranslationText, DetachedModelTranslationTextHighlighted, detachedGloss);
        DrawPdfTranslationLayout();
    }

    private static void HighlightTranslation(Label original, Label highlighted, string? gloss)
    {
        var text = original.Text ?? "";
        var formatted = new FormattedString();
        void AddSpan(string value, Color? background = null)
        {
            var span = new Span { Text = value, FontFamily = original.FontFamily, FontSize = original.FontSize };
            if (original.TextColor is { } foreground) span.TextColor = foreground;
            if (background is not null) span.BackgroundColor = background;
            formatted.Spans.Add(span);
        }
        var start = 0;
        if (!string.IsNullOrWhiteSpace(gloss))
        {
            var search = 0;
            while (search <= text.Length - gloss.Length)
            {
                var match = text.IndexOf(gloss, search, StringComparison.OrdinalIgnoreCase);
                if (match < 0) break;
                var end = match + gloss.Length;
                search = end;
                if ((match > 0 && char.IsLetterOrDigit(text[match - 1])) ||
                    (end < text.Length && char.IsLetterOrDigit(text[end]))) continue;
                if (match > start) AddSpan(text[start..match]);
                AddSpan(text[match..end], Color.FromArgb("#FFE48B"));
                start = end;
            }
        }
        if (start > 0 && start < text.Length) AddSpan(text[start..]);
        highlighted.FormattedText = start > 0 ? formatted : null;
        original.IsVisible = start == 0;
        highlighted.IsVisible = start > 0;
    }
}
