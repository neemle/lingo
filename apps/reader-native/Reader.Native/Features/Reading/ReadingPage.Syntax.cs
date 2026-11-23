using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private WordLookup? syntaxLookup;
    private int wordSyntaxRequest;
    private (DetachedAddress Address, string? Revision, TextSelection Selection, Task<WordLookup?> Pending)? detachedLookup;

    private void ShowLocalSyntax(WordLookup? lookup, bool loading = false)
    {
        wordSyntaxRequest++;
        syntaxLookup = lookup;
        WordSyntaxHint.Text = lookup?.SyntaxHint;
        WordSyntaxHint.IsVisible = lookup?.SyntaxHint is not null;
        WordSyntaxDetails.IsVisible = false;
        WordSyntaxModelPanel.IsVisible = false;
        WordSyntaxModelText.Text = "";
        OpenSyntax.Text = "Синтаксис";
        OpenSyntax.IsVisible = !loading;
    }

    private void ResetDetachedSyntax()
    {
        wordSyntaxRequest++;
        detachedLookup = null;
        SetDetachedSyntaxHint(null);
        DetachedSyntaxDetails.IsVisible = false;
        DetachedSyntaxModelPanel.IsVisible = false;
        DetachedSyntaxModelText.Text = "";
        DetachedOpenSyntax.Text = "Синтаксис";
    }

    private Task<WordLookup?> LookupDetachedCardAsync(DetachedAddress address, string? revision, TextSelection selected)
    {
        if (detachedLookup is { } lookup && ReferenceEquals(lookup.Address, address) &&
            lookup.Revision == revision && lookup.Selection == selected &&
            !lookup.Pending.IsFaulted && !lookup.Pending.IsCanceled) return lookup.Pending;
        var pending = selected.RejoinedWord is { } word
            ? AppState.Session.LookupWordAsync(address.Language, word)
            : AppState.Session.LookupDetachedPdfWordAsync(address.Document, address.Page, selected);
        detachedLookup = (address, revision, selected, pending);
        return pending;
    }

    private async Task LoadDetachedSyntaxHintAsync(DetachedAddress address, string? revision, TextSelection selected)
    {
        try
        {
            var lookup = await LookupDetachedCardAsync(address, revision, selected);
            if (readingPageActive && DetachedWordCard.IsVisible && ReferenceEquals(detachedAddress, address) &&
                detachedSentence?.RevisionId == revision && detachedSelection == selected)
            {
                SetDetachedSyntaxHint(lookup?.SyntaxHint);
                PositionWordPopup();
            }
        }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
    }

    private void SetDetachedSyntaxHint(string? hint)
    {
        DetachedSyntaxHint.Text = hint;
        DetachedSyntaxHint.IsVisible = hint is not null;
    }

    private async void OpenSyntaxClicked(object? sender, EventArgs args)
    {
        var detached = ReferenceEquals(sender, DetachedOpenSyntax);
        var card = detached ? DetachedWordCard : WordCard;
        var details = detached ? DetachedSyntaxDetails : WordSyntaxDetails;
        var button = detached ? DetachedOpenSyntax : OpenSyntax;
        var modelPanel = detached ? DetachedSyntaxModelPanel : WordSyntaxModelPanel;
        var hint = detached ? DetachedSyntaxHint : WordSyntaxHint;
        var selected = detached ? detachedSelection : selection;
        var session = AppState.Session;
        var anchor = CurrentAnalysisAnchor(session);
        if (selected is null || anchor is null || detached != anchor.Page.HasValue) return;

        var request = ++wordSyntaxRequest;
        details.IsVisible = !details.IsVisible;
        modelPanel.IsVisible = details.IsVisible;
        (detached ? DetachedSyntaxModelText : WordSyntaxModelText).Text = "";
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
                if (!hint.IsVisible && detachedAddress is { } address)
                {
                    var lookup = await LookupDetachedCardAsync(address, anchor.Revision, selected);
                    if (!Current()) return;
                    SetDetachedSyntaxHint(lookup?.SyntaxHint);
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
            else if (hint.IsVisible) details.Text = "";
            else details.Text = analysis is { Package.Installed: not true } or { Package: null }
                ? "Для синтаксичного розбору встановіть пакет аналізу цієї мови."
                : "Роль виділеного фрагмента в цьому реченні визначити не вдалося.";
        }
        catch (Exception error)
        {
            if (Current()) details.Text = error.Message;
        }
    }

    private async void ExplainSyntaxClicked(object? sender, EventArgs args)
    {
        if (sender is not Button button || !button.IsEnabled) return;
        var detached = ReferenceEquals(button, DetachedExplainSyntax);
        var details = detached ? DetachedSyntaxModelPanel : WordSyntaxModelPanel;
        var result = detached ? DetachedSyntaxModelText : WordSyntaxModelText;
        var card = detached ? DetachedWordCard : WordCard;
        var session = AppState.Session;
        var anchor = CurrentAnalysisAnchor(session);
        var selected = detached ? detachedSelection : selection;
        if (anchor is null || selected is null || detached != anchor.Page.HasValue) return;

        var request = wordSyntaxRequest;
        bool Current() => request == wordSyntaxRequest && readingPageActive && card.IsVisible && details.IsVisible &&
            CurrentAnalysisAnchor(session) == anchor && (detached ? detachedSelection : selection) == selected;
        button.IsEnabled = false;
        result.Text = "Шукаємо підказку Qwen…";
        try
        {
            var model = (await session.OptionalModelsAsync()).FirstOrDefault(model => model.CanGenerateLanguageHints);
            if (!Current()) return;
            if (model is null)
            {
                result.Text = "Qwen 2B недоступний.";
                return;
            }
            if (model.FilesPresent) result.Text = "Qwen пояснює синтаксис…";
            var explanation = await session.ReviewSentenceAsync(model.Id, anchor.DocumentId, anchor.Page,
                anchor.SentenceId, new { kind = "selection", startUtf16 = selected.StartUtf16, text = selected.Text },
                anchor.Revision, cachedOnly: !model.FilesPresent, explainSyntax: true);
            if (!Current()) return;
            result.Text = explanation switch
            {
                { Syntax: { } syntax } => $"Qwen · {syntax}",
                null when !model.FilesPresent => "Для нових підказок установіть Qwen 2B у налаштуваннях.",
                _ => "Qwen не зміг визначити роль виділення в цьому реченні.",
            };
        }
        catch (Exception error) { if (Current()) result.Text = error.Message; }
        finally { button.IsEnabled = true; }
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
