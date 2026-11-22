using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool findingInDocument;

    private async void FindInDocumentClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (findingInDocument || session.Busy || session.Document is not { } document) return;
        findingInDocument = true;
        try
        {
            if (await ShowDocumentFindAsync(document) is not { } position) return;
            await session.WaitUntilIdleAsync();
            if (!readingPageActive || session.Document is not { } current || current.Id != document.Id ||
                current.CurrentRevisionId != document.CurrentRevisionId ||
                position >= current.Sentences.Length || current.Sentences[position] != document.Sentences[position])
                return;

            StopSpeech();
            await session.SelectSentenceAsync(position);
        }
        catch (Exception error) { await DisplayAlertAsync("Пошук у документі", error.Message, "Гаразд"); }
        finally { findingInDocument = false; }
    }

    private async Task<int?> ShowDocumentFindAsync(Document document)
    {
        var resources = Application.Current!.Resources;
        var outline = (Style)resources["OutlineButton"];
        var entry = new Entry { Placeholder = "Слово або фраза", ReturnType = ReturnType.Search, MaxLength = 256 };
        SemanticProperties.SetDescription(entry, "Текст для пошуку");
        var search = new Button { Text = "Знайти", Style = (Style)resources["AccentButton"] };
        var previous = new Button { Text = "Попередні", Style = outline, IsVisible = false };
        var next = new Button { Text = "Наступні", Style = outline, IsVisible = false };
        var close = new Button { Text = "Закрити", Style = outline };
        var status = new Label { Text = "Введіть слово або фразу." };
        var results = new VerticalStackLayout { Spacing = 6 };
        var resultsScroll = new ScrollView { Content = results };

        var header = new Grid { ColumnDefinitions = [new(GridLength.Star), new(GridLength.Auto)], ColumnSpacing = 8 };
        header.Add(entry);
        header.Add(search, 1);
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
        var closed = new TaskCompletionSource<int?>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var cancellation = new CancellationTokenSource();
        var earlierPages = new Stack<int>();
        var query = "";
        var currentStart = 0;
        int? nextStart = null;
        var searching = false;
        page.Disappearing += (_, _) => closed.TrySetResult(null);
        page.Loaded += (_, _) => page.Dispatcher.Dispatch(() => entry.Focus());
        search.Clicked += async (_, _) => await StartSearchAsync();
        entry.Completed += async (_, _) => await StartSearchAsync();
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
            earlierPages.Clear();
            await ShowResultsAsync(0);
        }

        async Task ShowResultsAsync(int start)
        {
            searching = true;
            search.IsEnabled = previous.IsEnabled = next.IsEnabled = false;
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
                var matches = await Task.Run(() => DocumentFind.FindPage(document.Sentences, query, start, token), token);
                if (token.IsCancellationRequested || closed.Task.IsCompleted) return;
                nextStart = matches.NextStart;
                status.Text = matches.Positions.Length == 0 ? "Збігів не знайдено."
                    : $"Збіги · сторінка {earlierPages.Count + 1}";
                foreach (var position in matches.Positions)
                {
                    var result = new Button { Text = BookmarkLabel(position, document.Sentences[position].Text),
                        Style = outline, HorizontalOptions = LayoutOptions.Fill };
                    result.Clicked += (_, _) => closed.TrySetResult(position);
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
                    search.IsEnabled = true;
                    previous.IsVisible = previous.IsEnabled = earlierPages.Count > 0;
                    next.IsVisible = next.IsEnabled = nextStart.HasValue;
                }
            }
        }
    }
}
