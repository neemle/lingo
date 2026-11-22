using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool translatingSelection;
    private (string Document, string Revision, TextSelection Selection, string Target, string Text, string Source)? wordTranslation;
    private (string Target, string Text, string Source)? detachedWordTranslation;

    private async void TranslateSelectionClicked(object? sender, EventArgs args)
    {
        if (selection is not { } currentSelection || TargetCode is not { } target ||
            AppState.Session.Document is not { CurrentRevisionId: { } revision } document) return;
        await ShowSelectionTranslationAsync(document.Id, document.Language, revision, target, currentSelection, null,
            () => readingPageActive && selection == currentSelection && TargetCode == target &&
                  AppState.Session.Document is { } active && active.Id == document.Id && active.CurrentRevisionId == revision,
            result =>
            {
                Translation.Text = result.Text;
                wordTranslation = (document.Id, revision, currentSelection, target, result.Text,
                    result.SourceText ?? currentSelection.Text.Trim());
            });
    }

    private async void TranslateDetachedSelectionClicked(object? sender, EventArgs args)
    {
        if (detachedAddress is not { } address || detachedSentence?.RevisionId is not { } revision ||
            detachedSelection is not { } currentSelection || TargetCode is not { } target) return;
        await ShowSelectionTranslationAsync(address.Document, address.Language, revision, target, currentSelection, address.Page,
            () => readingPageActive && ReferenceEquals(detachedAddress, address) && detachedSelection == currentSelection &&
                  detachedSentence?.RevisionId == revision && TargetCode == target,
            result => detachedWordTranslation = (target, result.Text, result.SourceText ?? currentSelection.Text.Trim()));
    }

    private async Task ShowSelectionTranslationAsync(string id, string source, string revision, string target,
        TextSelection selected, int? page, Func<bool> current, Action<MachineTranslation> setDraft)
    {
        if (translatingSelection || AppState.Session.Busy) return;
        translatingSelection = true;
        try
        {
            var result = await AppState.Session.TranslateSelectionAsync(id, target, revision, selected, page);
            if (!current()) return;
            if (result is null)
            {
                await DisplayAlertAsync("Офлайн-переклад", "Для цієї мовної пари потрібні мовні пакети.", "Гаразд");
                return;
            }
            var route = result.PivotLanguage is { } pivot
                ? DocumentLanguages.PivotRoute(source, pivot, target)
                : DocumentLanguages.Name(target);
            if (await DisplayAlertAsync("Машинний переклад", $"«{result.SourceText ?? selected.Text}»{Environment.NewLine}{result.Text}{Environment.NewLine}{route}",
                "Використати як чернетку", "Закрити") && current())
                setDraft(result);
        }
        catch (Exception error)
        {
            if (current()) await DisplayAlertAsync("Офлайн-переклад", error.Message, "Гаразд");
        }
        finally { translatingSelection = false; }
    }
}
