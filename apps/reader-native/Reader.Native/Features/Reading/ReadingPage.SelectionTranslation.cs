using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private int popupTranslationRequest;
    private (string Document, string Revision, TextSelection Selection, string Target, string Text, string Source, bool Saved)? wordTranslation;
    private (string Target, string Text, string Source, bool Saved)? detachedWordTranslation;
    private sealed record GlossAlignment(string Target, string Gloss)
    {
        internal readonly Dictionary<string, OcrSpan[]> Resolved = new(StringComparer.Ordinal);
        internal readonly HashSet<string> Pending = new(StringComparer.Ordinal);
    }
    private GlossAlignment? glossAlignment;

    private async void TranslateSelectionClicked(object? sender, EventArgs args) =>
        await LoadPopupTranslationAsync(detached: false);

    private async void TranslateDetachedSelectionClicked(object? sender, EventArgs args) =>
        await LoadPopupTranslationAsync(detached: true);

    private async Task LoadPopupTranslationAsync(bool detached)
    {
        var request = ++popupTranslationRequest;
        var session = AppState.Session;
        var card = detached ? DetachedWordCard : WordCard;
        var selected = detached ? detachedSelection : selection;
        var document = session.Document;
        var address = detachedAddress;
        var revision = detached ? detachedSentence?.RevisionId : document?.CurrentRevisionId;
        var target = TargetCode;
        var source = detached ? address?.Language : document?.Language;
        if (!detached) wordTranslation = null;
        else detachedWordTranslation = null;
        if (selected is null || document is null || revision is null || target is null || target == source)
        {
            UpdatePopupGloss(detached, "Виберіть мову перекладу.");
            return;
        }

        bool Current() => request == popupTranslationRequest && readingPageActive && card.IsVisible &&
            TargetCode == target && session.Document?.Id == document.Id &&
            (detached
                ? ReferenceEquals(detachedAddress, address) && detachedSelection == selected && detachedSentence?.RevisionId == revision
                : selection == selected && session.Document?.CurrentRevisionId == revision);

        UpdatePopupGloss(detached, "Перекладаємо виділене…");
        try
        {
            await Task.Delay(120);
            if (!Current()) return;
            var result = await session.TranslateSelectionAsync(document.Id, target, revision, selected,
                detached ? address?.Page : null);
            if (!Current()) return;
            if (result is null)
            {
                UpdatePopupGloss(detached, "Для перекладу потрібні мовні пакети.");
                return;
            }

            var original = result.SourceText ?? selected.Text.Trim();
            if (detached) detachedWordTranslation = (target, result.Text, original, result.PersonalDictionary);
            else
            {
                wordTranslation = (document.Id, revision, selected, target, result.Text, original, result.PersonalDictionary);
                if (canonicalSense is null && string.IsNullOrWhiteSpace(Translation.Text)) Translation.Text = result.Text;
            }
            UpdatePopupGloss(detached);
        }
        catch (Exception error)
        {
            if (Current()) UpdatePopupGloss(detached, error.Message);
        }
    }

    private string? PopupMachineGloss(bool detached)
    {
        var target = TargetCode;
        if (detached)
            return detachedWordTranslation is { } pdf && pdf.Target == target ? pdf.Text : null;
        return wordTranslation is { } word && word.Target == target && word.Selection == selection &&
            AppState.Session.Document is { } document && document.Id == word.Document &&
            document.CurrentRevisionId == word.Revision ? word.Text : null;
    }

    private string? PopupGloss(bool detached)
    {
        var meaning = detached ? detachedSense : canonicalSense;
        return meaning is not null && meaning.Target == TargetCode
            ? meaning.Translation : PopupMachineGloss(detached);
    }

    private void UpdatePopupGloss(bool detached, string? status = null)
    {
        var gloss = PopupGloss(detached);
        var label = detached ? DetachedWordTranslationGloss : WordTranslationGloss;
        var origin = detached ? DetachedWordTranslationOrigin : WordTranslationOrigin;
        var listen = detached ? DetachedSpeakWordTranslation : SpeakWordTranslation;
        label.Text = gloss ?? status;
        var chosen = detached ? detachedSense : canonicalSense;
        origin.IsVisible = chosen?.Target != TargetCode && PopupMachineGloss(detached) is not null &&
            (detached ? detachedWordTranslation?.Saved : wordTranslation?.Saved) == true;
        listen.IsEnabled = !string.IsNullOrWhiteSpace(gloss);
        UpdatePopupTranslationHighlights();
    }

    private void PopupTranslationTextChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(Label.Text)) UpdatePopupTranslationHighlights();
    }

    private void UpdatePopupTranslationHighlights()
    {
        var canonicalGloss = WordCard.IsVisible ? PopupGloss(detached: false) : null;
        var detachedGloss = DetachedWordCard.IsVisible ? PopupGloss(detached: true) : null;
        if (canonicalGloss is null && detachedGloss is null) glossAlignment = null;
        if (DetachedPdfReader.IsVisible)
            QueueGlossAlignment(detachedGloss, [DetachedTranslation.Text, DetachedModelTranslationText.Text]);
        else
            QueueGlossAlignment(canonicalGloss, [SegmentTranslation.Text, ModelTranslationText.Text]);
        HighlightTranslation(SegmentTranslation, SegmentTranslationHighlighted, canonicalGloss);
        HighlightTranslation(ModelTranslationText, ModelTranslationTextHighlighted, canonicalGloss);
        HighlightTranslation(DetachedTranslation, DetachedTranslationHighlighted, detachedGloss);
        HighlightTranslation(DetachedModelTranslationText, DetachedModelTranslationTextHighlighted, detachedGloss);
        DrawPdfTranslationLayout();
    }

    private void QueueGlossAlignment(string? gloss, IEnumerable<string?> texts)
    {
        if (string.IsNullOrWhiteSpace(gloss) || TargetCode is not { } target ||
            System.Text.Encoding.UTF8.GetByteCount(gloss) > 512) return;
        if (glossAlignment is not { } state || state.Target != target || state.Gloss != gloss)
            glossAlignment = state = new(target, gloss);
        var batch = texts.OfType<string>()
            .Where(text => !state.Resolved.ContainsKey(text) && !state.Pending.Contains(text) &&
                text.Length > 0 && System.Text.Encoding.UTF8.GetByteCount(text) <= 8_000)
            .Distinct(StringComparer.Ordinal).Take(16).ToArray();
        if (batch.Length == 0) return;
        state.Pending.UnionWith(batch);
        _ = ResolveGlossAlignmentAsync(state, batch);
    }

    private async Task ResolveGlossAlignmentAsync(GlossAlignment state, string[] batch)
    {
        OcrSpan[][]? matches = null;
        try { matches = await AppState.Session.TranslationGlossMatchesAsync(state.Target, state.Gloss, batch); }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
        if (!ReferenceEquals(glossAlignment, state)) return;
        for (var index = 0; index < batch.Length; index++)
        {
            state.Pending.Remove(batch[index]);
            state.Resolved[batch[index]] = matches?.ElementAtOrDefault(index) ?? [];
        }
        if (readingPageActive) UpdatePopupTranslationHighlights();
    }

    private IEnumerable<(int Start, int End)> GlossRanges(string text, string? gloss)
    {
        if (string.IsNullOrWhiteSpace(gloss)) yield break;
        if (glossAlignment is { } state && state.Target == TargetCode && state.Gloss == gloss &&
            state.Resolved.TryGetValue(text, out var resolved) && resolved.Length > 0)
        {
            foreach (var match in resolved) yield return (match.StartUtf16, match.EndUtf16);
            yield break;
        }
        var search = 0;
        while (search <= text.Length - gloss.Length)
        {
            var match = text.IndexOf(gloss, search, StringComparison.OrdinalIgnoreCase);
            if (match < 0) break;
            var end = match + gloss.Length;
            search = end;
            if ((match > 0 && char.IsLetterOrDigit(text[match - 1])) ||
                (end < text.Length && char.IsLetterOrDigit(text[end]))) continue;
            yield return (match, end);
        }
    }

    private void HighlightTranslation(Label original, Label highlighted, string? gloss)
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
        foreach (var (match, end) in GlossRanges(text, gloss))
        {
            if (match < start || end <= match || end > text.Length) continue;
            if (match > start) AddSpan(text[start..match]);
            AddSpan(text[match..end], Color.FromArgb("#FFE48B"));
            start = end;
        }
        if (start > 0 && start < text.Length) AddSpan(text[start..]);
        highlighted.FormattedText = start > 0 ? formatted : null;
        original.IsVisible = start == 0;
        highlighted.IsVisible = start > 0;
    }
}
