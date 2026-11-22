using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private const int TranslationWindowLimit = 6;
    private readonly List<TranslationWindow> translationWindows = [];

    private sealed record TranslationScope(string Document, string Source, string Target);

    private sealed record AlignedUnit(int First, Sentence[] Sources, string? MachineText, bool JoinedLines,
        string? SourceSpan)
    {
        public bool Contains(int index) => index >= First && index < First + Sources.Length;

        public bool Matches(Document document)
        {
            if (First + Sources.Length > document.Sentences.Length) return false;
            for (var i = 0; i < Sources.Length; i++)
                if (Sources[i] != document.Sentences[First + i]) return false;

            return SourceSpan is null || document.Text.AsSpan(Sources[0].StartUtf16, SourceSpan.Length)
                .SequenceEqual(SourceSpan);
        }
    }

    private sealed record TranslationWindow(TranslationScope Scope, int Anchor, AlignedUnit[] Units,
        string? Pivot, bool IsParagraph, bool MoreBefore, bool MoreAfter)
    {
        public AlignedUnit? UnitFor(int position) => Array.Find(Units, unit => unit.Contains(position));
    }

    private IEnumerable<TranslationWindow> MatchingTranslationWindows(Document document, string target)
    {
        if (readiness is not { Ready: true } || readinessPair != (document.Language, target)) return [];
        var scope = new TranslationScope(document.Id, document.Language, target);
        return translationWindows.Where(window => window.Scope == scope && window.Pivot == readiness.PivotLanguage
            && window.UnitFor(document.Position) is { } unit && unit.Matches(document));
    }

    private TranslationWindow? FindTranslationWindow(Document document, string target)
    {
        var matching = MatchingTranslationWindows(document, target);
        return matching.FirstOrDefault(window => window.IsParagraph) ?? matching.FirstOrDefault();
    }

    private MachineTranslation? ActiveMachine(Document document, string target)
    {
        foreach (var window in MatchingTranslationWindows(document, target))
            if (window.UnitFor(document.Position) is { } unit && MachineFor(document, window, unit) is { } machine)
                return machine;
        return null;
    }

    private static string? ReviewedFor(Document document, string target, string sentenceId) =>
        document.ReviewedTranslations.GetValueOrDefault(target)?.GetValueOrDefault(sentenceId);

    private static MachineTranslation? MachineFor(Document document, TranslationWindow window, AlignedUnit unit)
    {
        if (unit.MachineText is not { } text || !unit.Matches(document)
            || unit.Sources.Any(source => ReviewedFor(document, window.Scope.Target, source.Id) is not null))
            return null;
        return new MachineTranslation(text, window.Pivot, unit.JoinedLines);
    }

    private bool HasUntranslatedUnits(Document document, TranslationWindow window) =>
        window.Units.Any(unit => unit.Matches(document)
            && unit.Sources.Any(source => ReviewedFor(document, window.Scope.Target, source.Id) is null)
            && MachineFor(document, window, unit) is null);

    private string? IndependentMachineText(Document document, string target) =>
        ActiveMachine(document, target) is { JoinedLines: false } machine ? machine.Text : null;

    private void RememberWindow(TranslationWindow window)
    {
        translationWindows.RemoveAll(previous => previous.Scope == window.Scope
            && previous.Anchor == window.Anchor && previous.IsParagraph == window.IsParagraph);
        translationWindows.Insert(0, window);
        if (translationWindows.Count > TranslationWindowLimit)
            translationWindows.RemoveAt(TranslationWindowLimit);
    }

    private static TranslationWindow BuildParagraphWindow(Document document, TranslationScope scope,
        ParagraphTranslation paragraph)
    {
        var ids = paragraph.Units.SelectMany(unit => unit.SentenceIds).ToHashSet(StringComparer.Ordinal);
        var indices = new Dictionary<string, int>(ids.Count, StringComparer.Ordinal);
        for (var i = 0; i < document.Sentences.Length; i++)
            if (ids.Contains(document.Sentences[i].Id)) indices.Add(document.Sentences[i].Id, i);

        var units = new List<AlignedUnit>(paragraph.Units.Length);
        foreach (var unit in paragraph.Units)
        {
            if (unit.SentenceIds.Length == 0 || !indices.TryGetValue(unit.SentenceIds[0], out var first)
                || unit.SentenceIds.Length > document.Sentences.Length - first)
                continue;
            var sources = document.Sentences.AsSpan(first, unit.SentenceIds.Length).ToArray();
            if (!sources.Select(source => source.Id).SequenceEqual(unit.SentenceIds, StringComparer.Ordinal))
                continue;
            var joined = sources.Length > 1;
            var span = joined ? document.Text[sources[0].StartUtf16..sources[^1].EndUtf16] : null;
            units.Add(new AlignedUnit(first, sources, unit.Reviewed ? null : unit.Translation, joined, span));
        }
        return new TranslationWindow(scope, document.Position, units.ToArray(),
            paragraph.PivotLanguage, true, paragraph.MoreBefore, paragraph.MoreAfter);
    }

    private async void TranslateOfflineClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (translating || session.Busy || readiness?.Ready != true || ActiveTranslationKey() is not { } key
            || session.Document is not { } document) return;

        translating = true;
        statusKey = key;
        machineStatus = "Перекладаємо абзац…";
        UpdateTranslation();
        var scope = new TranslationScope(key.Document, key.Source, key.Target);
        try
        {
            TranslationWindow? paragraph = null;
            try
            {
                var result = await session.TranslateParagraphAsync(key.Document, key.Sentence, key.Target);
                paragraph = BuildParagraphWindow(document, scope, result);
                if (paragraph.Units.Length > 0) RememberWindow(paragraph);
            }
            catch (Exception error)
            {
                if (ActiveTranslationKey() != key || ReviewedFor(document, key.Target, key.Sentence) is not null)
                    throw;
                machineStatus = error.Message;
            }

            if (ActiveTranslationKey() != key) return;
            if (session.ReviewedTranslation(key.Target) is not null
                || paragraph?.UnitFor(document.Position) is { } translated && session.Document is { } current
                   && MachineFor(current, paragraph, translated) is not null)
            {
                machineStatus = null;
                return;
            }

            var sentence = await session.TranslateSentenceAsync(key.Document, key.Sentence, key.Target);
            if (sentence is not null)
            {
                var unit = new AlignedUnit(document.Position, [document.Sentences[document.Position]],
                    sentence.Text, sentence.JoinedLines, null);
                RememberWindow(new TranslationWindow(scope, document.Position, [unit], sentence.PivotLanguage,
                    false, false, false));
            }
            else readinessPair = null;
            machineStatus = null;
        }
        catch (Exception error) { machineStatus = error.Message; }
        finally
        {
            translating = false;
            UpdateTranslation();
        }
    }

    private void UpdateSourceLocation(Document? document, Sentence? sentence)
    {
        if (document is null || sentence is null)
        {
            SourceLocation.IsVisible = false;
            return;
        }

        var block = Array.Find(document.Blocks, value => value.StartUtf16 <= sentence.StartUtf16
            && sentence.StartUtf16 < value.EndUtf16);
        var location = block?.Kind == "heading" ? "Заголовок"
            : block?.Page is int page ? $"Сторінка {page + 1}"
            : block is not null ? "Абзац" : "";
        SourceLocation.Text = location;
        SourceLocation.IsVisible = location.Length > 0;
    }

    private void UpdateParagraphPreview(Document? document, TranslationWindow? window, AlignedUnit? active)
    {
        ParagraphPreview.Children.Clear();
        ParagraphPreview.IsVisible = false;
        if (document is null || window is not { IsParagraph: true }) return;

        foreach (var unit in window.Units)
        {
            if (unit == active || !unit.Matches(document)) continue;
            if (MachineFor(document, window, unit) is { } translated)
                AddPreviewRow(document.Id, unit.First, unit.Sources[0].Id, unit.Sources.Length, translated.Text, reviewed: false);
            else
                for (var i = 0; i < unit.Sources.Length; i++)
                    if (ReviewedFor(document, window.Scope.Target, unit.Sources[i].Id) is { } review)
                        AddPreviewRow(document.Id, unit.First + i, unit.Sources[i].Id, 1, review, reviewed: true);
        }
        if (ParagraphPreview.Children.Count == 0) return;
        var caption = window.Pivot is { } pivot
            ? $"Переклади поруч · машинні через {DocumentLanguages.Name(pivot)}." : "Переклади поруч.";
        if (window.MoreBefore || window.MoreAfter)
            caption += " Виберіть інше речення, щоб перекласти ще частину абзацу.";
        ParagraphPreview.Children.Insert(0, new Label { Text = caption, FontSize = 13 });
        ParagraphPreview.IsVisible = true;
    }

    private void AddPreviewRow(string documentId, int position, string sentenceId, int count, string text, bool reviewed)
    {
        var positionLabel = count == 1 ? $"Речення {position + 1}" : $"Речення {position + 1}–{position + count}";
        if (reviewed) positionLabel += " · ваш переклад";
        var button = new Button { Text = positionLabel, FontSize = 13, HorizontalOptions = LayoutOptions.Start };
        button.Clicked += async (_, _) =>
        {
            var session = AppState.Session;
            if (session.Busy || session.Document is not { } current || current.Id != documentId
                || current.Sentences.ElementAtOrDefault(position)?.Id != sentenceId) return;
            StopSpeech();
            await session.SelectSentenceAsync(position);
        };
        ParagraphPreview.Children.Add(new VerticalStackLayout
        {
            Spacing = 3,
            Children = { button, new Label { Text = text, FontFamily = "Georgia",
                FontSize = Math.Max(16, SourceText.ReadingFontSize - 4), LineHeight = 1.4,
                TextColor = Color.FromArgb("#655F4D") } },
        });
    }
}
