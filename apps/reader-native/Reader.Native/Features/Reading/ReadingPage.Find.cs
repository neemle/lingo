using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool findingInDocument;
    private sealed record DocumentFindSelection(int Position, bool Translation,
        int? PhysicalPage = null, string? SentenceId = null, DetachedPdfTranslation? CachedMachine = null,
        string? Query = null, TranslationFindFragment? PdfFragment = null, string? FragmentTranslation = null);
    private sealed record DocumentFindFocus(string Document, string? Revision, int? Page,
        string Sentence, TextSelection? Span = null, string? Target = null, string? Query = null,
        TranslationFindFragment? PdfFragment = null, string? FragmentTranslation = null);
    private DocumentFindFocus? documentFindFocus;
#if MACCATALYST
    private sealed record UnreadPdfFind(string Document, int Page, string Query, int Occurrence);
    private UnreadPdfFind? pendingUnreadPdfFind;
#endif
    private sealed record DocumentFindCursor(int Position, int? PhysicalPage = null, bool UnreadLayer = false);
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
            ClearPdfFragmentFocus();
            documentFindFocus = null;
            if ((match.Translation && readingLayout == ReaderLayout.Source) ||
                (!match.Translation && readingLayout == ReaderLayout.Translation))
            {
                readingLayout = ReaderLayout.Automatic;
                ArrangeReadingColumns();
            }

#if MACCATALYST
            if (match.PhysicalPage is int unreadPage && match.SentenceId is null && !match.Translation)
            {
                await pdfWindowRead;
                if (!readingPageActive || session.Document?.Id != document.Id) return;
                await OpenPdfPageAsync(unreadPage);
                if (match.Query is { } query && readingPageActive && session.Document?.Id == document.Id &&
                    pdfWindow?.Center == unreadPage)
                {
                    pendingUnreadPdfFind = new(document.Id, unreadPage, query, match.Position);
                    ScheduleUnreadPdfFind();
                }
                return;
            }
            if (match.PhysicalPage is int page && match.SentenceId is { } sentenceId)
            {
                await pdfWindowRead;
                if (!readingPageActive || session.Document?.Id != document.Id) return;
                await OpenPdfPageAsync(page);
                if (detachedAddress is { } address && address.Document == document.Id && address.Page == page)
                {
                    await LoadDetachedSentenceAsync(address, match.Position, sentenceId);
                    if (ReferenceEquals(detachedAddress, address) && !loadingDetachedSentence &&
                        detachedSentence is { } found && found.Sentence.Id == sentenceId &&
                        !match.Translation && match.Query is { } query &&
                        DocumentFind.SourceMatch(found.Sentence, query) is { } span)
                        RevealDetachedFindSpan(address, found, span);
                    if (ReferenceEquals(detachedAddress, address) && !loadingDetachedSentence &&
                        detachedSentence is { } loaded && loaded.Sentence.Id == sentenceId &&
                        match.CachedMachine is { } cached && target is not null && TargetCode == target &&
                        ReviewedDetachedTranslation() is null)
                    {
                        ShowDetachedMachineTranslation(cached, loaded.Language, target);
                        UpdateDetachedControls();
                    }
                    if (ReferenceEquals(detachedAddress, address) && !loadingDetachedSentence &&
                        detachedSentence is { } selected && selected.Sentence.Id == sentenceId)
                        RevealTranslationFindMatch(match, address.Document, selected.RevisionId, page, sentenceId, target);
                }
                return;
            }
#endif
            var position = match.Position;
            if (current.CurrentRevisionId != document.CurrentRevisionId ||
                position < 0 || position >= current.Sentences.Length || position >= document.Sentences.Length ||
                current.Sentences[position] != document.Sentences[position]) return;
            if (!match.Translation && current.Format == "pdf" && current.CurrentRevisionId is { } revision &&
                match.Query is { } sourceQuery &&
                DocumentFind.SourceMatch(current.Sentences[position], sourceQuery) is { } sourceMatch)
            {
                StopSpeech();
                var cursor = await session.AnchorReadingSelectionAsync(current.Id, revision, sourceMatch);
                if (cursor is not null && readingPageActive && session.Document is { } selected &&
                    selected.Id == current.Id && selected.CurrentRevisionId == revision &&
                    session.Sentence?.Id == cursor.SentenceId)
                    RevealCanonicalFindSpan(current.Id, revision, cursor.SentenceId, sourceMatch);
                return;
            }
#if MACCATALYST
            if (current.Format == "pdf" && position == current.Position)
            {
                await pdfWindowRead;
                if (!readingPageActive || session.Document?.Id != document.Id) return;
                await OpenPdfPageAsync(match.PdfFragment?.Page ?? session.OriginalPageIndex());
            }
#endif
            StopSpeech();
            await session.SelectSentenceAsync(position, pdfPage: match.PdfFragment?.Page);
            if (readingPageActive && session.Error.Length == 0 && session.Document is { } opened &&
                opened.Id == document.Id && opened.CurrentRevisionId == current.CurrentRevisionId &&
                session.Sentence is { } sentence && sentence.Id == current.Sentences[position].Id)
            {
                if (match.Translation && opened.Format == "pdf")
                    RevealTranslationFindMatch(match, opened.Id, opened.CurrentRevisionId, null, sentence.Id, target);
                else if (!match.Translation &&
                    SetDocumentFindFocus(opened.Id, opened.CurrentRevisionId, null, sentence, match.Query) is { } foundSpan)
                {
                    UpdateHighlight();
                    SourceText.RevealOffset(foundSpan.StartUtf16);
                }
            }
        }
        catch (Exception error) { await DisplayAlertAsync("Пошук у документі", error.Message, "Гаразд"); }
        finally { findingInDocument = false; }
    }

    private TextSelection? SetDocumentFindFocus(string document, string? revision, int? page, Sentence sentence, string? query)
    {
        if (query is null || DocumentFind.SourceMatch(sentence, query) is not { } span) return null;
        documentFindFocus = new(document, revision, page, sentence.Id, span);
        return span;
    }

    private void RevealCanonicalFindSpan(string document, string revision, string sentence, TextSelection span)
    {
        documentFindFocus = new(document, revision, null, sentence, span);
        UpdateHighlight();
        SourceText.RevealOffset(span.StartUtf16);
    }

    private TextSelection? FindSourceFocus(string document, string? revision, Sentence sentence, int? page) =>
        documentFindFocus is { } focus && focus.Document == document && focus.Revision == revision &&
        focus.Sentence == sentence.Id && focus.Page == page &&
        (focus.Target is null || focus.Target == TargetCode) ? focus.Span : null;

    private void RevealTranslationFindMatch(DocumentFindSelection match, string document, string? revision,
        int? page, string sentence, string? target)
    {
        if (!match.Translation || match.Query is not { } query || target is null || TargetCode != target) return;
        var source = page is null ? AppState.Session.Document?.Text : detachedAddress?.Text;
        var span = match.PdfFragment is { } fragment && source is not null
            ? SourceRanges.Selection(source, fragment.StartUtf16, fragment.EndUtf16 - fragment.StartUtf16) : null;
        documentFindFocus = new(document, revision, page, sentence, Span: span, Target: target, Query: query,
            PdfFragment: match.PdfFragment, FragmentTranslation: span is null ? null : match.FragmentTranslation);
        if (span is not null && match.PdfFragment is { ReflowOnly: true })
        {
            pdfTranslationLayout = false;
            RefreshPdfTranslationLayout();
        }
        if (page is null)
        {
            UpdateHighlight();
            if (span is not null) SourceText.RevealOffset(span.StartUtf16);
        }
        else
        {
            if (span is not null)
            {
                DetachedPdfText.HighlightStart = span.StartUtf16;
                DetachedPdfText.HighlightLength = span.LengthUtf16;
                DetachedPdfText.RevealOffset(span.StartUtf16);
            }
            UpdateInlinePdfHighlight();
        }
        if (span is not null) RefreshFindTranslationDisplay();
    }

    private TranslationPair? FoundPdfTranslation(bool detached)
    {
        if (documentFindFocus is not { Span: { } source, FragmentTranslation: { } translation, Target: { } target } focus ||
            TargetCode != target || AppState.Session.Document is not { } document || focus.Document != document.Id)
            return null;
        var current = detached
            ? !loadingDetachedSentence && detachedAddress?.Page == focus.Page && detachedSentence is { } page &&
                page.RevisionId == focus.Revision && page.Sentence.Id == focus.Sentence
            : focus.Page is null && document.CurrentRevisionId == focus.Revision && AppState.Session.Sentence?.Id == focus.Sentence;
        return current ? new TranslationPair(source.Text, document.Language, translation, target,
            focus.PdfFragment?.PivotLanguage) : null;
    }

    private void RefreshFindTranslationDisplay()
    {
        if (DetachedPdfReader.IsVisible)
        {
            RenderDetachedTranslation();
            UpdateDetachedControls();
        }
        else UpdateTranslation();
    }

    private string? PdfTranslationFindQuery() =>
        documentFindFocus is { Target: not null } focus && pdfLayoutRequest is { } address &&
        focus.Document == address.Document && focus.Target == address.Target &&
        focus.Sentence == pdfLayoutSentence?.Id &&
        (address.DetachedText is null
            ? focus.Page is null && focus.Revision == address.Revision
            : focus.Page == address.Page && focus.Revision == detachedSentence?.RevisionId)
            ? focus.Query : null;

#if MACCATALYST
    private void ScheduleUnreadPdfFind()
    {
        if (pendingUnreadPdfFind is not { } find || pdfWindow?.Center != find.Page ||
            pdfWindow.Pages.FirstOrDefault(page => page.Page == find.Page) is not { } page ||
            page.State is "needsTextLayer" or "needsOcr" || page.RefreshPending && page.State == "empty")
            return;
        Dispatcher.Dispatch(async () =>
        {
            var session = AppState.Session;
            if (!ReferenceEquals(pendingUnreadPdfFind, find) || !readingPageActive ||
                pdfWindow?.Center != find.Page || session.Document is not { } document ||
                !ReferenceEquals(pdfSnapshot, document) || document.Id != find.Document ||
                !ReferenceEquals(pdfWindow.Pages.FirstOrDefault(item => item.Page == find.Page), page))
                return;
            pendingUnreadPdfFind = null;
            try
            {
                if (page.State == "detached" && detachedAddress is { } address &&
                    address.Document == find.Document && address.Page == find.Page &&
                    DocumentFind.SourceMatches(address.Text, find.Query).ElementAtOrDefault(find.Occurrence) is { } local)
                {
                    await LoadDetachedSentenceAsync(address, position: null, sourceOffset: local.StartUtf16);
                    if (readingPageActive && ReferenceEquals(detachedAddress, address) && !loadingDetachedSentence &&
                        detachedSentence is { } loaded && loaded.Sentence.StartUtf16 <= local.StartUtf16 &&
                        local.StartUtf16 < loaded.Sentence.EndUtf16)
                        RevealDetachedFindSpan(address, loaded, local);
                    return;
                }
                if (page.Span is not { } range || document.CurrentRevisionId is not { } revision ||
                    DocumentFind.SourceMatches(page.Text, find.Query, range.StartUtf16)
                        .ElementAtOrDefault(find.Occurrence) is not { } original ||
                    original.StartUtf16 > document.Text.Length - original.LengthUtf16 ||
                    !document.Text.AsSpan(original.StartUtf16, original.LengthUtf16).SequenceEqual(original.Text))
                    return;
                var cursor = await session.AnchorReadingSelectionAsync(document.Id, revision, original);
                if (cursor is null || !readingPageActive || session.Document is not { } current ||
                    current.Id != document.Id || current.CurrentRevisionId != revision || pdfWindow?.Center != find.Page ||
                    session.Sentence is not { } sentence || sentence.Id != cursor.SentenceId)
                    return;
                RevealCanonicalFindSpan(document.Id, revision, sentence.Id, original);
            }
            catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
        });
    }

    private void RevealDetachedFindSpan(DetachedAddress address, DetachedPdfSentence sentence, TextSelection span)
    {
        documentFindFocus = new(address.Document, sentence.RevisionId, address.Page, sentence.Sentence.Id, span);
        DetachedPdfText.HighlightStart = span.StartUtf16;
        DetachedPdfText.HighlightLength = span.LengthUtf16;
        DetachedPdfText.RevealOffset(span.StartUtf16);
        UpdateInlinePdfHighlight();
    }
#endif

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
        DescribeButton(search, "Знайти слово або фразу");
        DescribeButton(previous, "Показати попередні результати пошуку");
        DescribeButton(next, "Показати наступні результати пошуку");
        DescribeButton(close, "Закрити пошук");
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
#if MACCATALYST
        int[]? unreadPages = null;
#endif
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
#if MACCATALYST
            unreadPages = null;
#endif
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
                foreach (var match in page.Matches)
                {
                    var selection = match.Selection with { Query = query };
                    var text = selection is { PhysicalPage: not null, SentenceId: null } && !selection.Translation
                        ? match.Text : DocumentFind.MatchingExcerpt(match.Text, query);
                    var label = selection.PhysicalPage is int physicalPage
                        ? $"PDF {physicalPage + 1} · {(selection.SentenceId is null ? text : BookmarkLabel(selection.Position, text))}"
                        : BookmarkLabel(selection.Position, text);
                    var result = new Button { Text = label, Style = outline, HorizontalOptions = LayoutOptions.Fill };
                    ToolTipProperties.SetText(result, "Перейти до цього збігу в документі");
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
#if MACCATALYST
            if (start.UnreadLayer) return await AppendUnreadAsync(matches, start.PhysicalPage ?? 0, start.Position, token);
#endif
            if (start.PhysicalPage is null)
            {
                var found = await Task.Run(() => DocumentFind.FindPage(document.Sentences, query, start.Position, token), token);
                matches.AddRange(found.Positions.Select(position => new DocumentFindHit(document.Sentences[position].Text,
                    new DocumentFindSelection(position, Translation: false))));
                if (found.NextStart is int nextPosition)
                    return new(matches.ToArray(), new DocumentFindCursor(nextPosition));
            }
            var cached = await AppendDetachedAsync(matches, start, null, token);
#if MACCATALYST
            if (cached.Next is null && document.Format == "pdf")
                return await AppendUnreadAsync(matches, 0, 0, token);
#endif
            return cached;
        }

#if MACCATALYST
        async Task<DocumentFindPage> AppendUnreadAsync(List<DocumentFindHit> matches, int start, int occurrence,
            CancellationToken token)
        {
            unreadPages ??= await session.UnreadPdfTextPagesAsync(document.Id, token);
            var pending = unreadPages.Where(page => page >= start).ToArray();
            if (pending.Length == 0 || await session.OriginalFileAsync(document.Id) is not { } original)
                return new(matches.ToArray(), null);

            var space = DocumentFind.PageSize - matches.Count;
            var found = await PdfScan.FindUnreadTextAsync(original.Path, pending, pending[0] == start ? occurrence : 0,
                query, space + 1, token);
            matches.AddRange(found.Take(space).Select(hit => new DocumentFindHit(hit.Text,
                new DocumentFindSelection(hit.Occurrence, Translation: false, PhysicalPage: hit.Page))));
            var next = found.Length > space
                ? new DocumentFindCursor(found[space].Occurrence, found[space].Page, UnreadLayer: true) : null;
            return new(matches.ToArray(), next);
        }

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
                        CachedMachine: hit.Reviewed == false ? new DetachedPdfTranslation(hit.Text, hit.PivotLanguage, hit.SourceText, hit.Position) : null,
                        PdfFragment: hit.PdfFragment, FragmentTranslation: hit.PdfFragment is null ? null : hit.Text))));
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
                        .Select(hit => new DocumentFindHit(hit.Text, new DocumentFindSelection(hit.Position, Translation: true,
                            PdfFragment: hit.PdfFragment, FragmentTranslation: hit.PdfFragment is null ? null : hit.Text))));
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
