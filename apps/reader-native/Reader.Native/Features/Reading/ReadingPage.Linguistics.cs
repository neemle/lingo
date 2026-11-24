using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record AnalysisAnchor(string DocumentId, string Revision, string SentenceId, string Language, int? Page = null);
    private static readonly SemaphoreSlim AnalysisPrefetchGate = new(1, 1);
    private AnalysisAnchor? analysisPrefetchAnchor;
    private CancellationTokenSource? analysisPrefetchRun;

    private void SyncAnalysisPrefetch()
    {
        var session = AppState.Session;
        var anchor = readingPageActive ? CurrentAnalysisAnchor(session) : null;
        if (anchor is null)
        {
            StopAnalysisPrefetch();
            return;
        }
        if (session.Busy) return;

        if (analysisPrefetchAnchor == anchor) return;
        StopAnalysisPrefetch();
        analysisPrefetchAnchor = anchor;
        var run = new CancellationTokenSource();
        analysisPrefetchRun = run;
        _ = PrefetchAnalysisAsync(session, anchor, run);
    }

    private AnalysisAnchor? CurrentAnalysisAnchor(ReaderSession session)
    {
        if (PdfPageNotice.IsVisible)
            return !loadingDetachedSentence && detachedAddress is { } address &&
                detachedSentence is { RevisionId: { } pageRevision } detached &&
                session.Document?.Id == address.Document
                ? new(address.Document, pageRevision, detached.Sentence.Id, detached.Language, address.Page) : null;

        return session.Document is { CurrentRevisionId: { } revision } document && session.Sentence is { } sentence
            ? new(document.Id, revision, sentence.Id, document.Language) : null;
    }

    private async Task PrefetchAnalysisAsync(ReaderSession session, AnalysisAnchor anchor, CancellationTokenSource run)
    {
        var cancellation = run.Token;
        try
        {
            await Task.Delay(500, cancellation);
            await AnalysisPrefetchGate.WaitAsync(cancellation);
            try
            {
                foreach (var step in new[] { 0, 1, -1 })
                {
                    while (session.Busy || utteranceActive || translating || speakingDetachedSentence || translatingDetachedSentence)
                        await Task.Delay(250, cancellation);
                    cancellation.ThrowIfCancellationRequested();
                    if (analysisPrefetchAnchor != anchor) return;
                    await session.PrefetchLinguisticsAsync(
                        anchor.DocumentId, anchor.SentenceId, anchor.Revision, step, cancellation, anchor.Page);
                }
            }
            finally { AnalysisPrefetchGate.Release(); }
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) {}
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
        finally
        {
            if (analysisPrefetchRun == run) analysisPrefetchRun = null;
            run.Dispose();
        }
    }

    private void StopAnalysisPrefetch()
    {
        var run = analysisPrefetchRun;
        analysisPrefetchRun = null;
        analysisPrefetchAnchor = null;
        run?.Cancel();
    }

    private ExpressionMatch[] selectedExpressions = [];

    private static ExpressionSnapshot ExpressionCard(ExpressionMatch expression, KnowledgePackage package) =>
        new(expression.Canonical, expression.Text, expression.Kind, expression.Observations,
            expression.SourceName ?? package.Name, expression.SourceRevision ?? $"{package.Version} · {package.Revision}");

    private void ShowExpressionOptions(WordLookup? lookup, TextSelection selected)
    {
        selectedExpressions = lookup?.Expressions.Where(expression => expression.PartStartsUtf16.Length >= 2)
            .ToArray() ?? [];
        OpenExpressionAnalysis.IsVisible = selectedExpressions.Length > 0 || selected.Text.Any(char.IsWhiteSpace);
    }

    private async void OpenSentenceAnalysisClicked(object? sender, EventArgs args)
    {
        if (DetachedPdfReader.IsVisible)
        {
            await AnalyzeDetachedSentenceAsync();
            return;
        }
        var session = AppState.Session;
        if (PdfPageNotice.IsVisible || session.Busy || session.Document is not { CurrentRevisionId: { } revision } document
            || session.Sentence is not { } sentence) return;
        await ShowAnalysisPageAsync("Перевірка речення", sentence.Text,
            start => session.AnalyzeSentenceAsync(document.Id, sentence.Id, revision, start),
            (model, cachedOnly, cancellationToken) => session.ReviewSentenceAsync(model, document.Id, null, sentence.Id, revisionId: revision, cachedOnly: cachedOnly, cancellationToken: cancellationToken));
    }

    private async void OpenExpressionAnalysisClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (session.Busy || selection is not { } selected
            || session.Document is not { CurrentRevisionId: { } revision } document
            || session.Sentence is not { } sentence) return;
        var choices = selectedExpressions.Select(expression => (expression.Text, Members: expression.PartStartsUtf16)).ToList();
        try
        {
            if (selected.Text.Any(char.IsWhiteSpace) && selected.StartUtf16 >= sentence.StartUtf16 &&
                selected.StartUtf16 + selected.LengthUtf16 <= sentence.EndUtf16)
            {
                var members = await session.ReadExpressionMembersAsync(document.Id, sentence.Id, revision, selected);
                if (members is { Length: >= 2 and <= 8 } && !choices.Any(choice => choice.Members.SequenceEqual(members)))
                    choices.Add((selected.Text, members));
            }
            if (!AnalysisScopeIsCurrent(document, sentence, selected)) return;
            if (choices.Count == 0)
            {
                await DisplayAlertAsync("Аналіз виразу", "Виділіть від двох до восьми слів в одному реченні.", "Гаразд");
                return;
            }
            var choice = choices[0];
            if (choices.Count > 1)
            {
                var labels = choices.Select((entry, index) => $"{index + 1}. {entry.Text}").ToArray();
                var answer = await DisplayActionSheetAsync("Аналіз виразу", "Скасувати", null, labels);
                var index = Array.IndexOf(labels, answer);
                if (index < 0 || !AnalysisScopeIsCurrent(document, sentence, selected)) return;
                choice = choices[index];
            }
            await ShowAnalysisPageAsync("Перевірка виразу", choice.Text,
                _ => session.AnalyzeExpressionAsync(document.Id, sentence.Id, revision, choice.Members),
                (model, cachedOnly, cancellationToken) => session.ReviewSentenceAsync(model, document.Id, null, sentence.Id,
                    new { kind = "expression", memberStartsUtf16 = choice.Members }, revision, cachedOnly, cancellationToken: cancellationToken));
        }
        catch (Exception error)
        {
            if (AnalysisScopeIsCurrent(document, sentence, selected))
                await DisplayAlertAsync("Аналіз виразу", error.Message, "Гаразд");
        }
    }

    private bool AnalysisScopeIsCurrent(Document document, Sentence sentence, TextSelection selected) =>
        readingPageActive && selection == selected && AppState.Session.Document is { } current &&
        current.Id == document.Id && current.CurrentRevisionId == document.CurrentRevisionId &&
        AppState.Session.Sentence?.Id == sentence.Id;

    private async Task ShowAnalysisPageAsync(string title, string text, Func<int, Task<LinguisticAnalysis>> load,
        Func<string, bool, CancellationToken, Task<SentenceReview?>>? review = null)
    {
        if (!readingPageActive) return;
        StopAnalysisPrefetch();
        try { await Navigation.PushModalAsync(new AnalysisPage(title, text, load, review)); }
        catch (Exception error) { await DisplayAlertAsync(title, error.Message, "Гаразд"); }
    }

    private sealed class AnalysisPage : ContentPage
    {
        private readonly Func<int, Task<LinguisticAnalysis>> load;
        private readonly Label status = new();
        private readonly Func<string, bool, CancellationToken, Task<SentenceReview?>>? review;
        private readonly Button check = new() { Text = "Перевірити з Qwen", HorizontalOptions = LayoutOptions.Start };
        private readonly Label hints = new() { IsVisible = false };
        private readonly ActivityIndicator reviewBusy = new() { IsVisible = false, WidthRequest = 20, HeightRequest = 20,
            VerticalOptions = LayoutOptions.Start };
        private readonly VerticalStackLayout results = new() { Spacing = 9 };
        private readonly Button more = new() { Text = "Наступні слова", IsVisible = false };
        private readonly HashSet<(string Dependent, string Head, string Relation)> displayedLinks = [];
        private readonly HashSet<(int Start, int End, string Replacement)> displayedSpacing = [];
        private readonly HashSet<string> displayedHints = [];
        private int? nextStart = 0;
        private CancellationTokenSource? sentenceReview;
        private bool started;
        private bool loading;

        internal AnalysisPage(string title, string text, Func<int, Task<LinguisticAnalysis>> load,
            Func<string, bool, CancellationToken, Task<SentenceReview?>>? review)
        {
            Title = title;
            this.load = load;
            this.review = review;
            var close = new Button { Text = "Закрити" };
            DescribeButton(close, "Закрити мовний аналіз");
            close.Clicked += async (_, _) => await Navigation.PopModalAsync();
            more.Clicked += async (_, _) => await LoadAsync();
            var stack = new VerticalStackLayout { Padding = 24, Spacing = 12, MaximumWidthRequest = 760 };
            stack.Children.Add(new Label { Text = title, FontSize = 27, FontAttributes = FontAttributes.Bold });
            stack.Children.Add(new Label { Text = text, FontSize = 19 });
            stack.Children.Add(close);
            if (review is not null)
            {
                check.Clicked += async (_, _) => await ReviewLocallyAsync(cachedOnly: false);
                stack.Children.Add(check);
                var hintArea = new Grid
                {
                    ColumnSpacing = 8,
                    ColumnDefinitions = { new ColumnDefinition(GridLength.Auto), new ColumnDefinition(GridLength.Star) },
                };
                hintArea.Add(reviewBusy, 0, 0);
                hintArea.Add(hints, 1, 0);
                stack.Children.Add(hintArea);
            }
            stack.Children.Add(status);
            stack.Children.Add(results);
            stack.Children.Add(more);
            Content = new ScrollView { Content = stack };
        }

        private async Task ReviewLocallyAsync(bool cachedOnly)
        {
            if (!check.IsEnabled || review is null) return;
            using var request = new CancellationTokenSource();
            sentenceReview = request;
            var cancellation = request.Token;
            check.IsEnabled = false;
            if (!cachedOnly)
            {
                hints.IsVisible = true;
                hints.Text = "Шукаємо збережену перевірку…";
            }
            try
            {
                var available = (await AppState.Session.OptionalModelsAsync())
                    .FirstOrDefault(model => model.CanGenerateLanguageHints);
                cancellation.ThrowIfCancellationRequested();
                if (available is not null && await review(available.Id, true, cancellation) is { } cached)
                {
                    cancellation.ThrowIfCancellationRequested();
                    ShowReview(available.Name, cached);
                    return;
                }
                cancellation.ThrowIfCancellationRequested();
                if (cachedOnly) return;

                if (available is not { FilesPresent: true })
                {
                    hints.Text = "Установіть Qwen 2B у налаштуваннях. Підказки мовного пакета залишаються доступними.";
                    return;
                }
                reviewBusy.IsVisible = reviewBusy.IsRunning = true;
                hints.Text = $"{available.Name} перевіряє текст…";
                if (await review(available.Id, false, cancellation) is { } result && !cancellation.IsCancellationRequested)
                    ShowReview(available.Name, result);
            }
            catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { }
            catch (Exception error)
            {
                if (!cachedOnly && !cancellation.IsCancellationRequested) hints.Text = error.Message;
            }
            finally
            {
                sentenceReview = null;
                reviewBusy.IsRunning = reviewBusy.IsVisible = false;
                check.IsEnabled = true;
            }
        }

        private void ShowReview(string name, SentenceReview result)
        {
            var notes = new List<string> { result.Cached ? $"Збережені підказки {name}" : $"Підказки {name}" };
            if (result.Spelling is { } spelling) notes.Add($"Правопис · {spelling}");
            if (result.Syntax is { } syntax) notes.Add($"Синтаксис · {syntax}");
            if (notes.Count == 1) notes.Add("Модель не запропонувала виправлень.");
            hints.Text = string.Join(Environment.NewLine, notes);
            hints.IsVisible = true;
        }

        protected override void OnDisappearing()
        {
            sentenceReview?.Cancel();
            base.OnDisappearing();
        }

        protected override async void OnAppearing()
        {
            base.OnAppearing();
            if (started) return;
            started = true;
            _ = ReviewLocallyAsync(cachedOnly: true);
            await LoadAsync();
        }

        private async Task LoadAsync()
        {
            if (loading || nextStart is not int start) return;
            loading = true;
            more.IsEnabled = false;
            status.Text = "Аналізуємо текст…";
            try
            {
                var page = await load(start);
                status.Text = page.Package switch
                {
                    { DependencyParseAvailable: true } package => $"{package.Name} {package.Version}",
                    { Installed: true } package => $"{package.Name} {package.Version} · доступне оновлення індексу",
                    _ when page.SyntaxHints.Length > 0 || displayedHints.Count > 0 => "Синтаксичні підказки за правилами",
                    _ => "Мовний пакет недоступний. Встановіть його в налаштуваннях.",
                };
                foreach (var hint in page.SyntaxHints)
                    if (displayedHints.Add(hint)) Add(hint);
                if (page.SyntaxParse is { } parse) ShowParse(parse);
                foreach (var expression in page.Expressions)
                    Add(expression.Kind == "dictionary" ? $"«{expression.Text}» · словниковий вираз"
                        : $"«{expression.Text}» · сталий вираз · {expression.Observations} збігів");
                foreach (var suggestion in page.SpacingSuggestions)
                    if (displayedSpacing.Add((suggestion.Span.StartUtf16, suggestion.Span.EndUtf16, suggestion.Replacement)))
                        Add($"Пробіли · «{suggestion.Source}» → «{suggestion.Replacement}»");
                foreach (var word in page.Words) ShowWord(word);
                if (page.SyntaxParse is not { Links.Length: > 0 })
                    foreach (var link in page.SyntaxCandidates) ShowLink(link);
                nextStart = page.NextStart;
                more.Text = "Наступні слова";
            }
            catch (Exception error)
            {
                status.Text = error.Message;
                more.Text = "Спробувати ще раз";
            }
            finally
            {
                loading = false;
                more.IsEnabled = more.IsVisible = nextStart is not null;
            }
        }

        private void ShowParse(DependencyParse parse)
        {
            if (parse.RootText is { } root) Add($"Корінь речення · «{root}»");
            if (parse.Status == "partial")
                Add($"Частковий синтаксичний розбір · слів без зв’язку {parse.UnassignedTokenIds.Length}");
            else if (parse.Status == "noEvidence")
                Add("У корпусі бракує даних для дерева залежностей.");
            else if (parse.Status == "tooLong")
                Add("Для цього довгого речення доступні окремі синтаксичні підказки.");
            foreach (var link in parse.Links) ShowLink(link);
        }

        private void ShowWord(AnalyzedWord word)
        {
            var grammar = word.Morphology.Select(reading => $"{reading.Lemma} · {reading.PartOfSpeech}"
                + (reading.Features.Length == 0 ? "" : $" · {reading.Features.Replace("|", ", ")}"));
            var spell = word.SpellingStatus switch
            {
                "suggested" => string.Join(Environment.NewLine, word.SpellingSuggestions.Select(entry =>
                    $"«{entry.Replacement}» · {(entry.HighConfidence ? "Висока впевненість" : "Можливий варіант")}" +
                    $"{Environment.NewLine}У корпусі · {entry.Observations}, з попереднім словом · {entry.PreviousObservations}, " +
                    $"з наступним словом · {entry.NextObservations}")),
                "unattested" => "Словоформа відсутня в доступному корпусі",
                _ => "",
            };
            Add(string.Join('\n', new[] { $"«{word.Text}»" }.Concat(grammar).Append(spell)
                .Where(line => line.Length > 0)));
        }

        private void ShowLink(LinguisticSyntaxLink link)
        {
            if (!displayedLinks.Add((link.DependentTokenId, link.HeadTokenId, link.Relation))) return;
            var basis = link.Basis switch
            {
                "partOfSpeech" => " · за частинами мови",
                null => " · можливий зв’язок",
                _ => "",
            };
            Add($"«{link.DependentText}» → «{link.HeadText}» · {RelationName(link.Relation)}{basis}");
        }

        private void Add(string text) => results.Children.Add(new Label { Text = text, LineBreakMode = LineBreakMode.WordWrap });
    }
}
