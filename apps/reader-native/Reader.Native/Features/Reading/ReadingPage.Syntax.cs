using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordLookup? syntaxLookup;
    private int wordSyntaxRequest;

    private void ShowLocalSyntax(WordLookup? lookup)
    {
        wordSyntaxRequest++;
        syntaxLookup = lookup;
        WordSyntaxHint.Text = lookup?.SyntaxHint;
        WordSyntaxHint.IsVisible = lookup?.SyntaxHint is not null;
        WordSyntaxDetails.IsVisible = false;
        OpenSyntax.Text = "Синтаксис";
        OpenSyntax.IsVisible = lookup is { Package.DependencyParseAvailable: true } or { SyntaxEvidence.Length: > 0 };
    }

    private void ResetDetachedSyntax()
    {
        wordSyntaxRequest++;
        DetachedSyntaxDetails.IsVisible = false;
        DetachedOpenSyntax.Text = "Синтаксис";
    }

    private async void OpenSyntaxClicked(object? sender, EventArgs args)
    {
        var detached = ReferenceEquals(sender, DetachedOpenSyntax);
        var card = detached ? DetachedWordCard : WordCard;
        var details = detached ? DetachedSyntaxDetails : WordSyntaxDetails;
        var button = detached ? DetachedOpenSyntax : OpenSyntax;
        var hint = detached ? DetachedSyntaxHint : WordSyntaxHint;
        var selected = detached ? detachedSelection : selection;
        var session = AppState.Session;
        var anchor = CurrentAnalysisAnchor(session);
        if (selected is null || anchor is null || detached != anchor.Page.HasValue) return;

        var request = ++wordSyntaxRequest;
        details.IsVisible = !details.IsVisible;
        button.Text = details.IsVisible ? "Згорнути синтаксис" : "Синтаксис";
        if (!details.IsVisible) return;

        bool Current() => request == wordSyntaxRequest && readingPageActive && card.IsVisible &&
            CurrentAnalysisAnchor(session) == anchor && (detached ? detachedSelection : selection) == selected;

        details.Text = "Аналізуємо речення…";
        try
        {
            LinguisticAnalysis? analysis;
            if (anchor.Page is { } page)
            {
                if (!hint.IsVisible)
                {
                    var lookup = await session.LookupDetachedPdfWordAsync(anchor.DocumentId, page, selected);
                    if (!Current()) return;
                    hint.Text = lookup?.SyntaxHint;
                    hint.IsVisible = lookup?.SyntaxHint is not null;
                }
                analysis = await session.AnalyzeDetachedPdfSelectionAsync(anchor.DocumentId, page, selected);
            }
            else if (selected.Text.Any(char.IsWhiteSpace))
            {
                var members = await session.ReadExpressionMembersAsync(anchor.DocumentId, anchor.SentenceId, anchor.Revision, selected);
                if (!Current()) return;
                analysis = members.Length is >= 2 and <= 8
                    ? await session.AnalyzeExpressionAsync(anchor.DocumentId, anchor.SentenceId, anchor.Revision, members)
                    : null;
            }
            else if (session.Sentence is { } sentence && selected.StartUtf16 >= sentence.StartUtf16 &&
                selected.StartUtf16 < sentence.EndUtf16)
            {
                analysis = await session.AnalyzeWordAsync(anchor.DocumentId, anchor.SentenceId, anchor.Revision,
                    selected.StartUtf16 - sentence.StartUtf16);
            }
            else analysis = null;

            if (!Current()) return;
            var parse = analysis?.SyntaxParse;
            var lines = new List<string>();
            if (parse is { Links.Length: > 0 })
            {
                if (parse.RootText is { } root)
                    lines.Add($"«{root}» — головне слово речення.");
                lines.AddRange(parse.Links.Select(DescribeDependency).Distinct());
            }

            if (lines.Count == 0)
            {
                var candidates = analysis?.SyntaxCandidates.Select(DescribeDependency).ToArray() ?? [];
                if (candidates.Length > 0)
                {
                    lines.Add("Можливі зв’язки за корпусом");
                    lines.AddRange(candidates.Distinct());
                }
                else if (!detached && syntaxLookup is { SyntaxEvidence.Length: > 0 } lookup)
                {
                    lines.Add("Можливі зв’язки за корпусом");
                    lines.AddRange(lookup.SyntaxEvidence.Select(item =>
                        item.SelectedIsHead ? DependencyText(item.OtherWord, selected.Text, item.Relation)
                            : DependencyText(selected.Text, item.OtherWord, item.Relation)).Distinct());
                }
            }

            if (lines.Count > 0)
                details.Text = string.Join(Environment.NewLine, lines);
            else if (hint.IsVisible)
            {
                details.IsVisible = false;
                button.Text = "Синтаксис";
            }
            else details.Text = analysis is { Package.Installed: not true } or { Package: null }
                ? "Для синтаксичного розбору встановіть пакет аналізу цієї мови."
                : "Роль виділеного фрагмента в цьому реченні визначити не вдалося.";
        }
        catch (Exception error)
        {
            if (Current()) details.Text = error.Message;
        }
    }

    private static string DescribeDependency(LinguisticSyntaxLink link)
    {
        var line = DependencyText(link.DependentText, link.HeadText, link.Relation);
        return link.Basis == "partOfSpeech" ? $"{line} Припущення за частинами мови." : line;
    }

    private static string DependencyText(string dependent, string head, string relation) =>
        $"«{dependent}» — {RelationName(relation)} при «{head}».";

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
