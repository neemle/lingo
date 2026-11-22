using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool findingInDocument;
    private sealed record DocumentFindSelection(int Position, bool Translation);

    private async void FindInDocumentClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (findingInDocument || session.Busy || session.Document is not { } document) return;
        findingInDocument = true;
        try
        {
            var target = TargetCode;
            if (await ShowDocumentFindAsync(session, document, target) is not { } match) return;
            await session.WaitUntilIdleAsync();
            var position = match.Position;
            if (!readingPageActive || session.Document is not { } current || current.Id != document.Id ||
                current.CurrentRevisionId != document.CurrentRevisionId || current.Language != document.Language ||
                (match.Translation && TargetCode != target) ||
                position >= current.Sentences.Length || current.Sentences[position] != document.Sentences[position])
                return;

            if ((match.Translation && readingLayout == ReaderLayout.Source) ||
                (!match.Translation && readingLayout == ReaderLayout.Translation))
            {
                readingLayout = ReaderLayout.Automatic;
                ArrangeReadingColumns();
            }
            StopSpeech();
            await session.SelectSentenceAsync(position);
        }
        catch (Exception error) { await DisplayAlertAsync("Пошук у документі", error.Message, "Гаразд"); }
        finally { findingInDocument = false; }
    }

    private async Task<DocumentFindSelection?> ShowDocumentFindAsync(ReaderSession session, Document document, string? target)
    {
        var resources = Application.Current!.Resources;
        var outline = (Style)resources["OutlineButton"];
        var entry = new Entry { Placeholder = "Слово або фраза", ReturnType = ReturnType.Search, MaxLength = 256 };
        SemanticProperties.SetDescription(entry, "Текст для пошуку");
        var search = new Button { Text = "Знайти", Style = (Style)resources["AccentButton"] };
        var scope = new Picker
        {
            ItemsSource = new[] { "Оригінал", $"Збережений переклад · {DocumentLanguages.Name(target ?? "")}" },
            SelectedIndex = 0,
            IsVisible = target is not null,
        };
        SemanticProperties.SetDescription(scope, "Де шукати");
        var previous = new Button { Text = "Попередні", Style = outline, IsVisible = false };
        var next = new Button { Text = "Наступні", Style = outline, IsVisible = false };
        var close = new Button { Text = "Закрити", Style = outline };
        var status = new Label { Text = "Введіть слово або фразу." };
        var results = new VerticalStackLayout { Spacing = 6 };
        var resultsScroll = new ScrollView { Content = results };

        var header = new Grid
        {
            ColumnDefinitions = [new(GridLength.Star), new(GridLength.Auto)],
            RowDefinitions = [new(GridLength.Auto), new(GridLength.Auto)],
            ColumnSpacing = 8,
        };
        header.Add(entry);
        header.Add(search, 1);
        header.Add(scope, 0, 1);
        Grid.SetColumnSpan(scope, 2);
        var actions = new HorizontalStackLayout { Spacing = 8, Children = { previous, next, close } };
        var form = new Grid
        {
            Padding = 24,
            RowSpacing = 12,
            MaximumWidthRequest = 780,
            RowDefinitions = [new(GridLength.Auto), new(GridLength.Auto), new(GridLength.Auto),
                new(GridLength.Star), new(GridLength.Auto)],
        };
        var heading = new Label { Text = "Пошук у документі", Style = (Style)resources["SerifTitle"], FontSize = 30 };
        SemanticProperties.SetHeadingLevel(heading, SemanticHeadingLevel.Level1);
        form.Add(heading);
        form.Add(header, 0, 1);
        form.Add(status, 0, 2);
        form.Add(resultsScroll, 0, 3);
        form.Add(actions, 0, 4);
        var page = new ContentPage { Title = "Пошук у документі", Content = form };
        var closed = new TaskCompletionSource<DocumentFindSelection?>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var cancellation = new CancellationTokenSource();
        var earlierPages = new Stack<int>();
        var query = "";
        var inTranslation = false;
        var currentStart = 0;
        int? nextStart = null;
        var searching = false;
        page.Disappearing += (_, _) => closed.TrySetResult(null);
        page.Loaded += (_, _) => page.Dispatcher.Dispatch(() => entry.Focus());
        search.Clicked += async (_, _) => await StartSearchAsync();
        entry.Completed += async (_, _) => await StartSearchAsync();
        scope.SelectedIndexChanged += async (_, _) => await StartSearchAsync();
        previous.Clicked += async (_, _) =>
        {
            if (!searching && earlierPages.TryPop(out var start)) await ShowResultsAsync(start);
        };
        next.Clicked += async (_, _) =>
        {
            if (searching || nextStart is not { } start) return;
            earlierPages.Push(currentStart);
            await ShowResultsAsync(start);
        };
        close.Clicked += (_, _) => closed.TrySetResult(null);

        await Navigation.PushModalAsync(page);
        var selected = await closed.Task;
        cancellation.Cancel();
        if (Navigation.ModalStack.LastOrDefault() == page) await Navigation.PopModalAsync();
        return selected;

        async Task StartSearchAsync()
        {
            if (searching) return;
            query = entry.Text?.Trim() ?? "";
            inTranslation = target is not null && scope.SelectedIndex == 1;
            earlierPages.Clear();
            await ShowResultsAsync(0);
        }

        async Task ShowResultsAsync(int start)
        {
            searching = true;
            search.IsEnabled = scope.IsEnabled = previous.IsEnabled = next.IsEnabled = false;
            results.Children.Clear();
            nextStart = null;
            currentStart = start;
            var token = cancellation.Token;
            try
            {
                if (query.Length == 0)
                {
                    status.Text = "Введіть слово або фразу.";
                    return;
                }

                status.Text = "Шукаємо…";
                var page = inTranslation && target is not null
                    ? await FindTranslatedAsync(target, start, token)
                    : await FindOriginalAsync(start, token);
                if (token.IsCancellationRequested || closed.Task.IsCompleted) return;
                nextStart = page.NextStart;
                status.Text = page.Matches.Length > 0 ? $"Збіги · сторінка {earlierPages.Count + 1}"
                    : inTranslation ? "Збігів у збереженому перекладі немає." : "Збігів не знайдено.";
                foreach (var match in page.Matches)
                {
                    if (match.Position < 0 || match.Position >= document.Sentences.Length) continue;
                    var text = inTranslation ? DocumentFind.MatchingExcerpt(match.Text, query) : match.Text;
                    var result = new Button { Text = BookmarkLabel(match.Position, text),
                        Style = outline, HorizontalOptions = LayoutOptions.Fill };
                    var selection = new DocumentFindSelection(match.Position, inTranslation);
                    result.Clicked += (_, _) => closed.TrySetResult(selection);
                    results.Children.Add(result);
                }
                await resultsScroll.ScrollToAsync(0, 0, animated: false);
            }
            catch (OperationCanceledException) when (token.IsCancellationRequested) { }
            catch (Exception error) { if (!closed.Task.IsCompleted) status.Text = error.Message; }
            finally
            {
                searching = false;
                if (!closed.Task.IsCompleted)
                {
                    search.IsEnabled = scope.IsEnabled = true;
                    previous.IsVisible = previous.IsEnabled = earlierPages.Count > 0;
                    next.IsVisible = next.IsEnabled = nextStart.HasValue;
                }
            }
        }

        async Task<TranslationFindPage> FindOriginalAsync(int start, CancellationToken token)
        {
            var found = await Task.Run(() => DocumentFind.FindPage(document.Sentences, query, start, token), token);
            return new(found.Positions.Select(position => new TranslationFindHit(position, document.Sentences[position].Text))
                .ToArray(), found.NextStart);
        }

        async Task<TranslationFindPage> FindTranslatedAsync(string language, int start, CancellationToken token)
        {
            while (true)
            {
                token.ThrowIfCancellationRequested();
                var found = await session.FindTranslationPageAsync(document.Id, language, query, start, token);
                if (found.Matches.Length > 0 || found.NextStart is not int nextPosition ||
                    nextPosition <= start || nextPosition >= document.Sentences.Length)
                    return found;
                start = nextPosition;
            }
        }
    }
}
