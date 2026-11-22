using Reader.Contracts;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordLookup? syntaxLookup;

    private void ShowLocalSyntax(WordLookup? lookup)
    {
        syntaxLookup = lookup?.Package.SyntaxAvailable == true ? lookup : null;
        OpenSyntax.IsVisible = syntaxLookup is not null;
    }

    private async void OpenSyntaxClicked(object? sender, EventArgs args)
    {
        if (syntaxLookup is not { } lookup || selection is not { } selected)
            return;

        var lines = lookup.SyntaxEvidence.Select(hint =>
        {
            var dependent = hint.SelectedIsHead ? hint.OtherWord : selected.Text;
            var head = hint.SelectedIsHead ? selected.Text : hint.OtherWord;
            return $"«{dependent}» → «{head}» · {RelationName(hint.Relation)} ({hint.Relation}) · збігів у корпусі {hint.Observations}";
        }).ToArray();
        var source = $"{lookup.Package.Name} {lookup.Package.Version}";
        var details = lines.Length == 0
            ? "Для виділення та сусідніх слів у корпусі немає збігів."
            : string.Join("\n\n", lines);
        await DisplayAlertAsync("Синтаксичні підказки",
            $"{source}\nЗалежне слово → головне слово\n\n{details}", "Гаразд");
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
