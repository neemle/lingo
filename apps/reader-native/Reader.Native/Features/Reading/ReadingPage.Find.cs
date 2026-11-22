using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool findingInDocument;
    private sealed record DocumentFindSelection(int Position, bool Translation,
        int? PhysicalPage = null, string? SentenceId = null, DetachedPdfTranslation? CachedMachine = null);
    private sealed record DocumentFindCursor(int Position, int? PhysicalPage = null);
    private sealed record DocumentFindHit(string Text, DocumentFindSelection Selection);
    private sealed record DocumentFindPage(DocumentFindHit[] Matches, DocumentFindCursor? Next);

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
            if (!readingPageActive || session.Document is not { } current || current.Id != document.Id ||
                current.Language != document.Language || (match.Translation && TargetCode != target))
                return;

#if MACCATALYST
            if (match.PhysicalPage is int page && match.SentenceId is { } sentenceId)
            {
                await pdfWindowRead;
                if (!readingPageActive || session.Document?.Id != document.Id) return;
                await OpenPdfPageAsync(page);
                if (detachedAddress is { } address && address.Document == document.Id && address.Page == page)
                {
                    await LoadDetachedSentenceAsync(address, match.Position, sentenceId);
                    if (ReferenceEquals(detachedAddress, address) && !loadingDetachedSentence &&
                        detachedSentence is { } loaded && loaded.Sentence.Id == sentenceId &&
                        match.CachedMachine is { } cached && target is not null && TargetCode == target &&
                        ReviewedDetachedTranslation() is null)
                    {
                        ShowDetachedMachineTranslation(cached, loaded.Language, target);
                        UpdateDetachedControls();
                    }
                }
                return;
            }
#endif
            var position = match.Position;
            if (current.CurrentRevisionId != document.CurrentRevisionId ||
                position >= current.Sentences.Length || current.Sentences[position] != document.Sentences[position]) return;

            if ((match.Translation && readingLayout == ReaderLayout.Source) ||
                (!match.Translation && readingLayout == ReaderLayout.Translation))
            {
                readingLayout = ReaderLayout.Automatic;
                ArrangeReadingColumns();
            }
#if MACCATALYST
            if (current.Format == "pdf" && position == current.Position)
            {
                await pdfWindowRead;
                if (!readingPageActive || session.Document?.Id != document.Id) return;
                await OpenPdfPageAsync(session.OriginalPageIndex());
            }
#endif
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
        var earlierPages = new Stack<DocumentFindCursor>();
        var query = "";
        var inTranslation = false;
        var currentStart = new DocumentFindCursor(0);
        DocumentFindCursor? nextStart = null;
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
            await ShowResultsAsync(new DocumentFindCursor(0));
        }

        async Task ShowResultsAsync(DocumentFindCursor start)
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
                nextStart = page.Next;
                status.Text = page.Matches.Length > 0 ? $"Збіги · сторінка {earlierPages.Count + 1}"
                    : inTranslation ? "Збігів у збереженому перекладі немає." : "Збігів не знайдено.";
                if (!inTranslation && document.PdfPendingTextPages.Length > 0)
                    status.Text += " Пошук охоплює сторінки з доступним текстом.";
                foreach (var match in page.Matches)
                {
                    var selection = match.Selection;
                    var text = selection.Translation ? DocumentFind.MatchingExcerpt(match.Text, query) : match.Text;
                    var label = BookmarkLabel(selection.Position, text);
                    if (selection.PhysicalPage is int physicalPage) label = $"PDF {physicalPage + 1} · {label}";
                    var result = new Button { Text = label, Style = outline, HorizontalOptions = LayoutOptions.Fill };
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
                    next.IsVisible = next.IsEnabled = nextStart is not null;
                }
            }
        }

        async Task<DocumentFindPage> FindOriginalAsync(DocumentFindCursor start, CancellationToken token)
        {
            var matches = new List<DocumentFindHit>(DocumentFind.PageSize);
            if (start.PhysicalPage is null)
            {
                var found = await Task.Run(() => DocumentFind.FindPage(document.Sentences, query, start.Position, token), token);
                matches.AddRange(found.Positions.Select(position => new DocumentFindHit(document.Sentences[position].Text,
                    new DocumentFindSelection(position, Translation: false))));
                if (found.NextStart is int nextPosition)
                    return new(matches.ToArray(), new DocumentFindCursor(nextPosition));
            }
            return await AppendDetachedAsync(matches, start, null, token);
        }

#if MACCATALYST
        async Task<DocumentFindPage> AppendDetachedAsync(
            List<DocumentFindHit> matches, DocumentFindCursor start, string? language, CancellationToken token)
        {
            if (document.Format != "pdf") return new(matches.ToArray(), null);
            var cursor = new DetachedPdfFindCursor(start.PhysicalPage ?? 0,
                start.PhysicalPage is null ? 0 : start.Position);
            var space = DocumentFind.PageSize - matches.Count;
            while (true)
            {
                token.ThrowIfCancellationRequested();
                var found = await session.FindDetachedPdfTextAsync(
                    document.Id, query, cursor, Math.Max(1, space), token, language);
                if (space == 0 && found.Matches.FirstOrDefault() is { } first)
                    return new(matches.ToArray(), new DocumentFindCursor(first.Position, first.Page));

                matches.AddRange(found.Matches.Select(hit => new DocumentFindHit(hit.Text,
                    new DocumentFindSelection(hit.Position, Translation: language is not null,
                        PhysicalPage: hit.Page, SentenceId: hit.SentenceId,
                        CachedMachine: hit.Reviewed == false ? new DetachedPdfTranslation(hit.Text, hit.PivotLanguage, hit.SourceText, hit.Position) : null))));
                if (found.Next is not { } next || next == cursor) return new(matches.ToArray(), null);
                space = DocumentFind.PageSize - matches.Count;
                cursor = next;
            }
        }
#else
        static Task<DocumentFindPage> AppendDetachedAsync(
            List<DocumentFindHit> matches, DocumentFindCursor start, string? language, CancellationToken token) =>
            Task.FromResult(new DocumentFindPage(matches.ToArray(), null));
#endif

        async Task<DocumentFindPage> FindTranslatedAsync(string language, DocumentFindCursor start, CancellationToken token)
        {
            var matches = new List<DocumentFindHit>(DocumentFind.PageSize);
            if (start.PhysicalPage is null)
            {
                var position = start.Position;
                while (true)
                {
                    token.ThrowIfCancellationRequested();
                    var found = await session.FindTranslationPageAsync(document.Id, language, query, position, token);
                    matches.AddRange(found.Matches.Where(hit => hit.Position >= 0 && hit.Position < document.Sentences.Length)
                        .Select(hit => new DocumentFindHit(hit.Text, new DocumentFindSelection(hit.Position, Translation: true))));
                    if (found.NextStart is not int nextPosition ||
                        nextPosition <= position || nextPosition >= document.Sentences.Length)
                        break;
                    if (matches.Count > 0)
                        return new(matches.ToArray(), new DocumentFindCursor(nextPosition));
                    position = nextPosition;
                }
            }
            return await AppendDetachedAsync(matches, start, language, token);
        }
    }
}
