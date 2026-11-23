using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool sharingReaderText;

    private sealed record TranslationPair(string Source, string SourceLanguage, string Target, string TargetLanguage);

    private TranslationPair? DisplayedTranslationPair()
    {
        if (PdfUsesPageTranslation) return DisplayedPdfTranslationPair();
        var session = AppState.Session;
        var shown = SegmentTranslation.Text;
        if (session.Document is not { } document || session.Sentence is not { } sentence
            || TargetCode is not { } target || string.IsNullOrWhiteSpace(shown))
            return null;

        if (session.ReviewedTranslation(target) == shown)
            return new TranslationPair(sentence.Text, document.Language, shown, target);

        if (ActiveMachineAlignment(document, target) is not { } aligned
            || aligned.Translation.Text != shown)
            return null;

        // Older sentence-only model results can report joined lines without an exact source span.
        var source = aligned.Unit.JoinedLines ? aligned.Unit.SourceSpan : sentence.Text;
        return source is null ? null : new TranslationPair(source, document.Language, shown, target);
    }

    private TranslationPair? DisplayedPdfTranslationPair()
    {
        if (!readingPageActive || !PdfUsesPageTranslation || CurrentPdfLayoutAddress() is not { } address ||
            address != pdfLayoutRequest || AppState.Session.Document is not { } document)
            return null;

        var selected = DetachedPdfReader.IsVisible
            ? !loadingDetachedSentence && detachedAddress?.Page == address.Page ? detachedSentence?.Sentence : null
            : AppState.Session.Sentence;
        if (selected is null) return null;
        var row = Array.Find(pdfLayoutRows, candidate => candidate.StartUtf16 < selected.EndUtf16 &&
            candidate.EndUtf16 > selected.StartUtf16 && !string.IsNullOrWhiteSpace(candidate.Text) &&
            !string.IsNullOrWhiteSpace(candidate.SourceText));
        return row is { SourceText: { } original, Text: { } translated }
            ? new TranslationPair(original, document.Language, translated, address.Target) : null;
    }

    private TranslationPair? DisplayedDetachedTranslationPair()
    {
        if (PdfUsesPageTranslation) return DisplayedPdfTranslationPair();
        if (detachedSentence is not { } current || TargetCode is not { } target ||
            CurrentDetachedTranslation() is not { } translated) return null;
        var source = ReviewedDetachedTranslation() is null ? detachedMachineTranslation?.SourceText : null;
        return new TranslationPair(source ?? current.Sentence.Text, current.Language, translated, target);
    }

    private async void ShareSelectionClicked(object? sender, EventArgs args)
    {
        if (sharingReaderText) return;
        if (AppState.Session.Document is not { } document || selection is not { } selected
            || !ShareableSelection(document.Text, selected))
        {
            await DisplayAlertAsync("Поширити уривок", "Виділіть уривок у відкритому документі.", "Гаразд");
            return;
        }

        await ShareReaderTextAsync("Виділений уривок", selected.Text);
    }

    private async void DetachedShareSelectionClicked(object? sender, EventArgs args)
    {
        if (sharingReaderText || detachedSelection is not { } selected || !CanShareDetachedSelection(selected)) return;
        await ShareReaderTextAsync("Виділений уривок", selected.Text);
    }

    private bool CanShareDetachedSelection(TextSelection selected) =>
        !loadingDetachedSentence && detachedAddress is { } address && DetachedPdfReader.IsVisible &&
        AppState.Session.Document?.Id == address.Document && DetachedPdfText.Text == address.Text &&
        ShareableSelection(address.Text, selected);

    private static bool ShareableSelection(string text, TextSelection selected) =>
        selected.StartUtf16 >= 0 && selected.LengthUtf16 > 0 &&
        selected.StartUtf16 <= text.Length - selected.LengthUtf16 &&
        !string.IsNullOrWhiteSpace(selected.Text) &&
        text.AsSpan(selected.StartUtf16, selected.LengthUtf16).SequenceEqual(selected.Text);

    private async void ShareTranslationPairClicked(object? sender, EventArgs args)
    {
        if (sharingReaderText || !TranslationVisible || DisplayedTranslationPair() is not { } pair) return;
        await ShareTranslationPairAsync(pair);
    }

    private Task ShareTranslationPairAsync(TranslationPair pair)
    {
        var sourceLabel = DocumentLanguages.Name(pair.SourceLanguage);
        var targetLabel = DocumentLanguages.Name(pair.TargetLanguage);
        var text = $"{sourceLabel}{Environment.NewLine}{pair.Source}{Environment.NewLine}{Environment.NewLine}" +
                   $"{targetLabel}{Environment.NewLine}{pair.Target}";
        return ShareReaderTextAsync("Оригінал і переклад", text);
    }

    private async Task ShareReaderTextAsync(string title, string text)
    {
        sharingReaderText = true;
        try { await Share.Default.RequestAsync(new ShareTextRequest(text, title)); }
        catch (Exception error) { await DisplayAlertAsync(title, error.Message, "Гаразд"); }
        finally { sharingReaderText = false; }
    }
}
