using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool sharingReaderText;

    private sealed record TranslationPair(string Source, string SourceLanguage, string Target, string TargetLanguage);

    private TranslationPair? DisplayedTranslationPair()
    {
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

    private async void ShareSelectionClicked(object? sender, EventArgs args)
    {
        if (sharingReaderText) return;
        if (AppState.Session.Document is not { } document || selection is not { } selected
            || selected.StartUtf16 < 0 || selected.LengthUtf16 <= 0
            || selected.StartUtf16 > document.Text.Length - selected.LengthUtf16
            || !document.Text.AsSpan(selected.StartUtf16, selected.LengthUtf16).SequenceEqual(selected.Text)
            || string.IsNullOrWhiteSpace(selected.Text))
        {
            await DisplayAlertAsync("Поширити уривок", "Виділіть уривок у відкритому документі.", "Гаразд");
            return;
        }

        await ShareReaderTextAsync("Виділений уривок", selected.Text);
    }

    private async void ShareTranslationPairClicked(object? sender, EventArgs args)
    {
        if (sharingReaderText || !TranslationVisible || DisplayedTranslationPair() is not { } pair) return;
        var sourceLabel = DocumentLanguages.Name(pair.SourceLanguage);
        var targetLabel = DocumentLanguages.Name(pair.TargetLanguage);
        var text = $"{sourceLabel}{Environment.NewLine}{pair.Source}{Environment.NewLine}{Environment.NewLine}" +
                   $"{targetLabel}{Environment.NewLine}{pair.Target}";
        await ShareReaderTextAsync("Оригінал і переклад", text);
    }

    private async Task ShareReaderTextAsync(string title, string text)
    {
        sharingReaderText = true;
        try { await Share.Default.RequestAsync(new ShareTextRequest(text, title)); }
        catch (Exception error) { await DisplayAlertAsync(title, error.Message, "Гаразд"); }
        finally { sharingReaderText = false; }
    }
}
