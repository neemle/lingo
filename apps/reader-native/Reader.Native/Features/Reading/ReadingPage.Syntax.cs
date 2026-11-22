using Reader.Contracts;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordLookup? syntaxLookup;

    private void ShowLocalSyntax(WordLookup? lookup)
    {
        syntaxLookup = lookup;
        WordSyntaxHint.Text = lookup?.SyntaxHint;
        WordSyntaxHint.IsVisible = lookup?.SyntaxHint is not null;
        WordSyntaxDetails.IsVisible = false;
        OpenSyntax.Text = "Синтаксис";
        OpenSyntax.IsVisible = lookup is { SyntaxEvidence.Length: > 0 };
    }

    private void OpenSyntaxClicked(object? sender, EventArgs args)
    {
        if (syntaxLookup is not { } lookup || selection is not { } selected)
            return;
        WordSyntaxDetails.IsVisible = !WordSyntaxDetails.IsVisible;
        OpenSyntax.Text = WordSyntaxDetails.IsVisible ? "Згорнути синтаксис" : "Синтаксис";
        if (!WordSyntaxDetails.IsVisible) return;

        var lines = lookup.SyntaxEvidence.Select(hint =>
        {
            var dependent = hint.SelectedIsHead ? hint.OtherWord : selected.Text;
            var head = hint.SelectedIsHead ? selected.Text : hint.OtherWord;
            return $"«{dependent}» → «{head}» · {RelationName(hint.Relation)}";
        }).ToArray();
        var source = $"{lookup.Package.Name} {lookup.Package.Version}";
        WordSyntaxDetails.Text = $"Зв’язки, засвідчені в корпусі {source}\n{string.Join("\n", lines)}";
    }

    private static string RelationName(string relation) => relation.Split(':')[0] switch
    {
        "acl" => "реченнєве означення",
        "advcl" => "підрядна обставина",
        "advmod" => "прислівниковий поширювач",
        "amod" => "прикметникове означення",
        "appos" => "прикладка",
        "aux" => "допоміжне дієслово",
        "case" => "відмінковий показник",
        "cc" => "сурядний сполучник",
        "ccomp" => "підрядне доповнення",
        "compound" => "складений вираз",
        "conj" => "сурядний зв’язок",
        "cop" => "дієслово-зв’язка",
        "csubj" => "підрядний підмет",
        "det" => "артикль або визначник",
        "expl" => "формальний підмет або додаток",
        "fixed" => "сталий вираз",
        "flat" => "складена назва",
        "iobj" => "непрямий додаток",
        "mark" => "показник підрядності",
        "nmod" => "іменникове означення",
        "nsubj" => "підмет",
        "nummod" => "числівникове означення",
        "obj" => "прямий додаток",
        "obl" => "непрямий іменниковий компонент",
        "xcomp" => "предикативне доповнення",
        _ => "залежність",
    };
}
