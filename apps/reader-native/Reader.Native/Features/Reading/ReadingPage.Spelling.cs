using Reader.Contracts;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordLookup? spellingLookup;

    private void ShowSpelling(WordLookup? lookup)
    {
        spellingLookup = lookup is { SpellingSuggestions.Length: > 0 } ? lookup : null;
        OpenSpelling.IsVisible = spellingLookup is not null;
    }

    private async void OpenSpellingClicked(object? sender, EventArgs args)
    {
        if (spellingLookup is not { } lookup || selection is not { } selected)
            return;

        var lines = lookup.SpellingSuggestions.Select(suggestion =>
            $"«{suggestion.Replacement}» · {(suggestion.HighConfidence ? "Висока впевненість" : "Можливий варіант")}" +
            $"\nУ корпусі: {suggestion.Observations} · з попереднім словом: {suggestion.PreviousObservations}" +
            $" · з наступним словом: {suggestion.NextObservations}");
        var source = $"{lookup.Package.Name} {lookup.Package.Version} · {lookup.Package.License}";
        await DisplayAlertAsync("Підказки правопису", $"«{selected.Text}»\n{source}\n\n{string.Join("\n\n", lines)}", "Гаразд");
    }
}
