using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private async void OpenSpellingClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (session.Busy || selection is not { } selected || session.Sentence is not { } sentence ||
            session.Document is not { CurrentRevisionId: { } revision } document) return;
        try
        {
            var members = await session.ReadExpressionMembersAsync(document.Id, sentence.Id, revision, selected);
            if (members is not [var start] || !AnalysisScopeIsCurrent(document, sentence, selected)) return;
            await ShowAnalysisPageAsync("Перевірка слова", selected.Text,
                _ => session.AnalyzeWordAsync(document.Id, sentence.Id, revision, start),
                model => session.ReviewSentenceAsync(model, document.Id, null, sentence.Id,
                    new { kind = "selection", startUtf16 = selected.StartUtf16, text = selected.Text }, revision));
        }
        catch (Exception error)
        {
            if (AnalysisScopeIsCurrent(document, sentence, selected))
                await DisplayAlertAsync("Перевірка слова", error.Message, "Гаразд");
        }
    }
}
