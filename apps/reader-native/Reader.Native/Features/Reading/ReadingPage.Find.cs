using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool findingInDocument;
    private sealed record DocumentFindSelection(int Position, bool Translation,
        int? PhysicalPage = null, string? SentenceId = null, DetachedPdfTranslation? CachedMachine = null,
        string? Query = null, TranslationFindFragment? PdfFragment = null, string? SavedText = null,
        TranslationAlignment? Alignment = null, string? PivotLanguage = null, string? SourceText = null,
        TextSelection? SourceMatch = null, string? SourcePageRevision = null);
    private sealed record DocumentFindFocus(string Document, string? Revision, int? Page,
        string Sentence, TextSelection? Span = null, string? Target = null, string? Query = null,
        TranslationFindFragment? PdfFragment = null, TranslationPair? Translation = null);
    private DocumentFindFocus? documentFindFocus;
#if MACCATALYST || IOS
    private sealed record UnreadPdfFind(string Document, int Page, string Query, int Occurrence);
    private UnreadPdfFind? pendingUnreadPdfFind;
#endif
    private sealed record DocumentFindCursor(int Position, int? PhysicalPage = null, bool UnreadLayer = false,
        int Offset = 0);
    private sealed record DocumentFindHit(string Text, DocumentFindSelection Selection);
    private sealed record DocumentFindPage(DocumentFindHit[] Matches, DocumentFindCursor? Next, bool NeedsOcr = false);

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
            if (match.Translation && target is not null)
            {
                var latest = await RefreshTranslationFindMatchAsync(session, document, match, target);
                if (!readingPageActive || session.Document is not { } stillOpen || stillOpen.Id != document.Id ||
                    stillOpen.Language != document.Language || TargetCode != target) return;
                if (latest is null)
                {
                    await DisplayAlertAsync("Пошук у документі",
                        "Цей збіг змінився. Повторіть пошук, щоб побачити актуальний переклад.", "Гаразд");
                    return;
                }
                current = stillOpen;
                match = latest;
            }
            ClearTranslationFocus();
            documentFindFocus = null;
            if ((match.Translation && readingLayout == ReaderLayout.Source) ||
                (!match.Translation && readingLayout == ReaderLayout.Translation))
            {
                readingLayout = ReaderLayout.Automatic;
                ArrangeReadingColumns();
            }

            if (match.SourceMatch is { } exact && match.SourcePageRevision is null)
            {
                if (current.CurrentRevisionId is not { } exactRevision || exactRevision != document.CurrentRevisionId ||
                    exact.StartUtf16 > current.Text.Length - exact.LengthUtf16 ||
                    !current.Text.AsSpan(exact.StartUtf16, exact.LengthUtf16).SequenceEqual(exact.Text))
                {
                    await DisplayAlertAsync("Пошук у документі",
                        "Текст змінився. Повторіть пошук, щоб побачити актуальні збіги.", "Гаразд");
                    return;
                }
                await AnchorCanonicalFindSpanAsync(session, current.Id, exactRevision, exact);
                return;
            }

#if MACCATALYST || IOS
            if (match.SourceMatch is { } detachedMatch && match.SourcePageRevision is { } pageRevision &&
                match.PhysicalPage is int sourcePage)
            {
                await pdfWindowRead;
                if (!readingPageActive || session.Document?.Id != document.Id) return;
                await OpenPdfPageAsync(sourcePage);
                if (!readingPageActive || session.Document?.Id != document.Id || pdfWindow?.Center != sourcePage) return;
                if (detachedAddress is { } sourceAddress && sourceAddress.Document == document.Id &&
                    sourceAddress.Page == sourcePage && sourceAddress.Language == document.Language)
                {
                    await detachedInitialSentence;
                    if (!ReferenceEquals(detachedAddress, sourceAddress) || !readingPageActive) return;
                    if (detachedSentence?.RevisionId == pageRevision &&
                        detachedMatch.StartUtf16 <= sourceAddress.Text.Length - detachedMatch.LengthUtf16 &&
                        sourceAddress.Text.AsSpan(detachedMatch.StartUtf16, detachedMatch.LengthUtf16).SequenceEqual(detachedMatch.Text))
                    {
                        await LoadDetachedSentenceAsync(sourceAddress, position: null, sourceOffset: detachedMatch.StartUtf16);
                        if (ReferenceEquals(detachedAddress, sourceAddress) && readingPageActive && !loadingDetachedSentence &&
                            detachedSentence is { } loaded && loaded.RevisionId == pageRevision &&
                            loaded.Sentence.StartUtf16 <= detachedMatch.StartUtf16 && detachedMatch.StartUtf16 < loaded.Sentence.EndUtf16)
                            RevealDetachedFindSpan(sourceAddress, loaded, detachedMatch);
                        return;
                    }
                }
                await DisplayAlertAsync("Пошук у документі",
                    "Текст змінився. Повторіть пошук, щоб побачити актуальні збіги.", "Гаразд");
                return;
            }
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
                        DocumentFind.PdfSourceMatches(found.Sentence.Text, query, found.Language,
                            found.Sentence.StartUtf16).FirstOrDefault() is { } span)
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
                DocumentFind.PdfSourceMatches(current.Sentences[position].Text, sourceQuery, current.Language,
                    current.Sentences[position].StartUtf16).FirstOrDefault() is { } sourceMatch)
            {
                await AnchorCanonicalFindSpanAsync(session, current.Id, revision, sourceMatch);
                return;
            }
#if MACCATALYST || IOS
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
                if (match.Translation)
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

    private static bool MatchesFindSource(TranslationFindPage results, Document document) =>
        results.SourceRevisionId == document.CurrentRevisionId && results.SourceLanguage == document.Language;

    private async Task AnchorCanonicalFindSpanAsync(ReaderSession session, string document, string revision, TextSelection match)
    {
        StopSpeech();
        var cursor = await session.AnchorReadingSelectionAsync(document, revision, match);
        if (cursor is not null && readingPageActive && session.Document is { } selected &&
            selected.Id == document && selected.CurrentRevisionId == revision && session.Sentence?.Id == cursor.SentenceId)
            RevealCanonicalFindSpan(document, revision, cursor.SentenceId, match);
    }

    private static bool SameFindFragment(TranslationFindFragment? previous, TranslationFindFragment? latest) =>
        previous is null ? latest is null : latest is not null &&
        previous.Page == latest.Page && previous.StartUtf16 == latest.StartUtf16 &&
        previous.EndUtf16 == latest.EndUtf16 && previous.ReflowOnly == latest.ReflowOnly;

    private static int FindFragmentOffset(TranslationFindFragment? fragment) =>
        fragment is { ReflowOnly: false } ? fragment.StartUtf16 : 0;

    private static async Task<DocumentFindSelection?> RefreshTranslationFindMatchAsync(
        ReaderSession session, Document document, DocumentFindSelection match, string target)
    {
        if (match.Query is not { } query) return null;
        if (match.PhysicalPage is int page)
        {
            var results = await session.FindDetachedPdfTextAsync(document.Id, query,
                new DetachedPdfFindCursor(page, match.Position, FindFragmentOffset(match.PdfFragment)), 1, default, target);
            var hit = results.Matches.FirstOrDefault(hit => hit.Page == page && hit.Position == match.Position &&
                hit.SentenceId == match.SentenceId && SameFindFragment(match.PdfFragment, hit.PdfFragment));
            return hit is null ? null : match with
            {
                SavedText = hit.Text,
                PdfFragment = hit.PdfFragment,
                CachedMachine = CachedFindMachine(hit),
                SourceText = hit.SourceText,
            };
        }

        var found = await session.FindTranslationPageAsync(document.Id, target, query, match.Position, default,
            FindFragmentOffset(match.PdfFragment));
        if (!MatchesFindSource(found, document)) return null;
        var current = found.Matches.FirstOrDefault(hit => hit.Position == match.Position &&
            SameFindFragment(match.PdfFragment, hit.PdfFragment));
        return current is null ? null : match with
        {
            SavedText = current.Text, Alignment = current.Alignment,
            PdfFragment = current.PdfFragment, PivotLanguage = current.PivotLanguage,
            SourceText = current.SourceText,
        };
    }

    private static DetachedPdfTranslation? CachedFindMachine(DetachedPdfFindHit hit) =>
        hit.Reviewed == false ? new(hit.Text, hit.PivotLanguage, hit.SourceText, hit.Position) : null;

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
        var session = AppState.Session;
        if (!match.Translation || match.Query is not { } query || match.SavedText is not { } saved ||
            target is null || TargetCode != target || session.Document is not { } opened || opened.Id != document)
            return;
        var selected = page is null ? session.Sentence : detachedSentence?.Sentence;
        var source = page is null ? opened.Text : detachedAddress?.Text;
        if (selected is null || source is null) return;
        var span = match.PdfFragment is { } fragment
            ? SourceRanges.Selection(source, fragment.StartUtf16, fragment.EndUtf16 - fragment.StartUtf16)
            : null;
        if (match.PdfFragment is not null && span is null) return;
        if (match.Alignment is not null)
        {
            if (SourceRanges.ResolveTranslationSource(opened, match.Alignment) is not { } range) return;
            var first = opened.Sentences[range.First];
            var last = opened.Sentences[range.First + range.Count - 1];
            span = SourceRanges.Selection(source, first.StartUtf16, last.EndUtf16 - first.StartUtf16);
            if (span is null) return;
        }
        var original = match.SourceText ?? span?.Text ?? selected.Text;
        var pivot = match.PdfFragment?.PivotLanguage ?? match.CachedMachine?.PivotLanguage ?? match.PivotLanguage;
        var pair = new TranslationPair(original, opened.Language, saved, target, pivot is null ? null : [pivot]);
        documentFindFocus = new(document, revision, page, sentence, Span: span, Target: target, Query: query,
            PdfFragment: match.PdfFragment, Translation: pair);
        if (PdfUsesPageTranslation && (match.PdfFragment is { ReflowOnly: true } ||
            match.PdfFragment is null && !PageShowsSavedTranslation(selected, saved)))
        {
            pdfTranslationLayout = false;
            RefreshPdfTranslationLayout();
        }
        else if (PdfUsesPageTranslation && match.PdfFragment is { } fragmentHit &&
            !PageShowsSavedTranslation(selected with { StartUtf16 = fragmentHit.StartUtf16, EndUtf16 = fragmentHit.EndUtf16 }, saved))
        {
            RefreshCachedPdfPageLayout();
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
        RefreshFindTranslationDisplay();
        if (page is null) RequestTranslationScroll();
    }

    private bool PageShowsSavedTranslation(Sentence selected, string saved) =>
        pdfLayoutRequest is not null && pdfLayoutRequest == CurrentPdfLayoutAddress() &&
        pdfLayoutRows.Any(row => IsActivePdfRow(row, selected) && row.Text == saved);

    private TranslationPair? FoundTranslation(bool detached)
    {
        if (documentFindFocus is not { Translation: { } pair } focus || TargetCode != pair.TargetLanguage ||
            AppState.Session.Document is not { } document || focus.Document != document.Id ||
            pair.SourceLanguage != document.Language)
            return null;
        var current = detached
            ? !loadingDetachedSentence && detachedAddress?.Page == focus.Page && detachedSentence is { } page &&
                page.RevisionId == focus.Revision && page.Sentence.Id == focus.Sentence
            : focus.Page is null && document.CurrentRevisionId == focus.Revision && AppState.Session.Sentence?.Id == focus.Sentence;
        return current ? pair : null;
    }

    private string? FoundTranslationQuery(bool detached) =>
        FoundTranslation(detached) is not null ? documentFindFocus?.Query : null;

    private void RefreshFindTranslationDisplay()
    {
        if (DetachedPdfReader.IsVisible)
        {
            RenderDetachedTranslation();
            UpdateDetachedControls();
        }
        else UpdateTranslation();
        UpdatePopupTranslationHighlights();
    }

    private string? PdfTranslationFindQuery() =>
        documentFindFocus is { Target: not null } focus && pdfLayoutRequest is { } address &&
        focus.Document == address.Document && focus.Target == address.Target &&
        focus.Sentence == pdfLayoutSentence?.Id &&
        (address.DetachedText is null
            ? focus.Page is null && focus.Revision == address.Revision
            : focus.Page == address.Page && focus.Revision == detachedSentence?.RevisionId)
            ? focus.Query : null;

#if MACCATALYST || IOS
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
                    DocumentFind.PdfSourceMatches(address.Text, find.Query, address.Language)
                        .ElementAtOrDefault(find.Occurrence) is { } local)
                {
                    await LoadDetachedSentenceAsync(address, position: null, sourceOffset: local.StartUtf16);
                    if (readingPageActive && ReferenceEquals(detachedAddress, address) && !loadingDetachedSentence &&
                        detachedSentence is { } loaded && loaded.Sentence.StartUtf16 <= local.StartUtf16 &&
                        local.StartUtf16 < loaded.Sentence.EndUtf16)
                        RevealDetachedFindSpan(address, loaded, local);
                    return;
                }
                if (page.Span is not { } range || document.CurrentRevisionId is not { } revision ||
                    DocumentFind.PdfSourceMatches(page.Text, find.Query, document.Language, range.StartUtf16)
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
        CancellationTokenSource? activeSearch = null;
        var earlierPages = new Stack<DocumentFindCursor>();
        var query = "";
        var inTranslation = false;
        var currentStart = new DocumentFindCursor(0);
        DocumentFindCursor? nextStart = null;
#if MACCATALYST || IOS
        int[]? unreadPages = null;
#endif
        page.Disappearing += (_, _) => closed.TrySetResult(null);
        page.Loaded += (_, _) => page.Dispatcher.Dispatch(() => entry.Focus());
        search.Clicked += async (_, _) => await StartSearchAsync();
        entry.Completed += async (_, _) => await StartSearchAsync();
        entry.TextChanged += (_, _) =>
        {
            if (closed.Task.IsCompleted || (entry.Text?.Trim() ?? "") == query) return;
            ResetSearchResults();
            status.Text = string.IsNullOrWhiteSpace(entry.Text)
                ? "Введіть слово або фразу." : "Натисніть «Знайти».";
        };
        scope.SelectedIndexChanged += async (_, _) => await StartSearchAsync();
        previous.Clicked += async (_, _) =>
        {
            if (activeSearch is null && earlierPages.TryPop(out var start)) await ShowResultsAsync(start);
        };
        next.Clicked += async (_, _) =>
        {
            if (activeSearch is not null || nextStart is not { } start) return;
            earlierPages.Push(currentStart);
            await ShowResultsAsync(start);
        };
        close.Clicked += (_, _) => closed.TrySetResult(null);

        await Navigation.PushModalAsync(page);
        var selected = await closed.Task;
        activeSearch?.Cancel();
        if (Navigation.ModalStack.LastOrDefault() == page) await Navigation.PopModalAsync();
        return selected;

        async Task StartSearchAsync()
        {
            if (closed.Task.IsCompleted) return;
            ResetSearchResults();
            query = entry.Text?.Trim() ?? "";
            inTranslation = target is not null && scope.SelectedIndex == 1;
            await ShowResultsAsync(new DocumentFindCursor(0));
        }

        void ResetSearchResults()
        {
            activeSearch?.Cancel();
            activeSearch = null;
            results.Children.Clear();
            nextStart = null;
            earlierPages.Clear();
            previous.IsVisible = previous.IsEnabled = next.IsVisible = next.IsEnabled = false;
#if MACCATALYST || IOS
            unreadPages = null;
#endif
        }

        async Task ShowResultsAsync(DocumentFindCursor start)
        {
            using var request = new CancellationTokenSource();
            activeSearch = request;
            previous.IsEnabled = next.IsEnabled = false;
            results.Children.Clear();
            nextStart = null;
            currentStart = start;
            var token = request.Token;
            var searchQuery = query;
            var searchTranslation = inTranslation;
            bool Current() => activeSearch == request && !token.IsCancellationRequested && !closed.Task.IsCompleted;
            try
            {
                if (searchQuery.Length == 0)
                {
                    status.Text = "Введіть слово або фразу.";
                    return;
                }

                status.Text = "Шукаємо…";
                var page = searchTranslation && target is not null
                    ? await FindTranslatedAsync(searchQuery, target, start, token)
                    : await FindOriginalAsync(searchQuery, start, token);
                if (!Current()) return;
                nextStart = page.Next;
                if (page.Matches.Length > 0)
                    status.Text = $"Збіги · сторінка {earlierPages.Count + 1}";
                else if (searchTranslation)
                    status.Text = "Збігів у збереженому перекладі немає.";
                else
                {
                    var needsOcr = document.Format == "pdf" && (page.NeedsOcr ||
                        await session.HasUnrecognizedPdfSearchPagesAsync(document.Id, token));
                    if (!Current()) return;
                    status.Text = needsOcr
                        ? "У доступному тексті збігів немає. Відкрийте нерозпізнані сторінки, щоб додати їхній текст до пошуку."
                        : "Збігів не знайдено.";
                }
                foreach (var match in page.Matches)
                {
                    var selection = match.Selection with { Query = searchQuery };
                    var text = match.Text;
                    if (selection.Translation || selection.PhysicalPage is null || selection.SentenceId is not null)
                    {
                        var sourceMatch = !selection.Translation && document.Format == "pdf"
                            ? DocumentFind.PdfSourceMatches(text, searchQuery, document.Language).FirstOrDefault()
                            : DocumentFind.SourceMatch(text, searchQuery);
                        if (sourceMatch is not null) text = DocumentFind.MatchingExcerpt(text, sourceMatch, keepOpening: true);
                    }
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
            catch (Exception error) { if (Current()) status.Text = error.Message; }
            finally
            {
                if (activeSearch == request)
                {
                    activeSearch = null;
                    if (!closed.Task.IsCompleted)
                    {
                        previous.IsVisible = previous.IsEnabled = earlierPages.Count > 0;
                        next.IsVisible = next.IsEnabled = nextStart is not null;
                    }
                }
            }
        }

        async Task<DocumentFindPage> FindOriginalAsync(string query, DocumentFindCursor start, CancellationToken token)
        {
            var matches = new List<DocumentFindHit>(DocumentFind.PageSize);
#if MACCATALYST || IOS
            if (start.UnreadLayer) return await AppendUnreadAsync(matches, query, start.PhysicalPage ?? 0, start.Position, token);
#endif
            if (start.PhysicalPage is null)
            {
                if (document is { Format: "pdf", PdfPageStarts: not null })
                {
                    var (hits, next) = await Task.Run(
                        () => DocumentFind.FindPdfPage(document, query, start.Position, start.Offset, token), token);
                    matches.AddRange(hits.Select(hit => new DocumentFindHit(hit.Text,
                        new DocumentFindSelection(0, Translation: false, PhysicalPage: hit.Page, SourceMatch: hit.Selection))));
                    if (next is { } continuation)
                        return new(matches.ToArray(), new DocumentFindCursor(continuation.Page, Offset: continuation.Offset));
                }
                else
                {
                    var pdfLanguage = document.Format == "pdf" ? document.Language : null;
                    var found = await Task.Run(() => DocumentFind.FindPage(document.Sentences, query, start.Position, token, pdfLanguage), token);
                    matches.AddRange(found.Positions.Select(position => new DocumentFindHit(document.Sentences[position].Text,
                        new DocumentFindSelection(position, Translation: false))));
                    if (found.NextStart is int nextPosition)
                        return new(matches.ToArray(), new DocumentFindCursor(nextPosition));
                }
            }
#if MACCATALYST || IOS
            if (document.Format == "pdf")
            {
                var cached = await AppendDetachedOriginalAsync(matches, query, start, token);
                return cached.Next is not null ? cached : await AppendUnreadAsync(matches, query, 0, 0, token);
            }
#endif
            return new(matches.ToArray(), null);
        }

#if MACCATALYST || IOS
        async Task<DocumentFindPage> AppendUnreadAsync(List<DocumentFindHit> matches, string query, int start, int occurrence,
            CancellationToken token)
        {
            var pages = unreadPages ?? await session.UnreadPdfTextPagesAsync(document.Id, token);
            token.ThrowIfCancellationRequested();
            unreadPages = pages;
            var pending = pages.Where(page => page >= start).ToArray();
            if (pending.Length == 0 || await session.OriginalFileAsync(document.Id) is not { } original)
                return new(matches.ToArray(), null);

            var space = DocumentFind.PageSize - matches.Count;
            var (found, needsOcr) = await PdfScan.FindUnreadTextAsync(original.Path, pending, pending[0] == start ? occurrence : 0,
                query, document.Language, space + 1, token);
            matches.AddRange(found.Take(space).Select(hit => new DocumentFindHit(hit.Text,
                new DocumentFindSelection(hit.Occurrence, Translation: false, PhysicalPage: hit.Page))));
            var next = found.Length > space
                ? new DocumentFindCursor(found[space].Occurrence, found[space].Page, UnreadLayer: true) : null;
            return new(matches.ToArray(), next, needsOcr);
        }

        async Task<DocumentFindPage> AppendDetachedOriginalAsync(
            List<DocumentFindHit> matches, string query, DocumentFindCursor start, CancellationToken token)
        {
            var startPage = start.PhysicalPage ?? 0;
            var minimum = start.PhysicalPage is null ? 0 : start.Offset;
            while (true)
            {
                var batch = await session.ReadDetachedPdfSearchPagesAsync(document.Id, startPage, token);
                var space = DocumentFind.PageSize - matches.Count;
                var found = await Task.Run(() => batch.Pages.SelectMany(page =>
                    DocumentFind.PdfPageHits(page.Page, page.Text, query, document.Language, 0,
                        page.Page == startPage ? minimum : 0, token)
                        .Select(hit => (Hit: hit, page.RevisionId))).Take(space + 1).ToArray(), token);
                matches.AddRange(found.Take(space).Select(result => new DocumentFindHit(result.Hit.Text,
                    new DocumentFindSelection(0, Translation: false, PhysicalPage: result.Hit.Page,
                        SourceMatch: result.Hit.Selection, SourcePageRevision: result.RevisionId))));
                if (found.Length > space)
                {
                    var nextHit = found[space].Hit;
                    return new(matches.ToArray(), new DocumentFindCursor(0, nextHit.Page, Offset: nextHit.Selection.StartUtf16));
                }
                if (batch.NextPage is not int next || next <= startPage) return new(matches.ToArray(), null);
                startPage = next;
                minimum = 0;
            }
        }

        async Task<DocumentFindPage> AppendDetachedTranslationsAsync(
            List<DocumentFindHit> matches, string query, DocumentFindCursor start, string language, CancellationToken token)
        {
            if (document.Format != "pdf") return new(matches.ToArray(), null);
            var cursor = new DetachedPdfFindCursor(start.PhysicalPage ?? 0,
                start.PhysicalPage is null ? 0 : start.Position, start.PhysicalPage is null ? 0 : start.Offset);
            var space = DocumentFind.PageSize - matches.Count;
            while (true)
            {
                token.ThrowIfCancellationRequested();
                var found = await session.FindDetachedPdfTextAsync(
                    document.Id, query, cursor, Math.Max(1, space), token, language);
                if (space == 0 && found.Matches.FirstOrDefault() is { } first)
                    return new(matches.ToArray(), new DocumentFindCursor(first.Position, first.Page,
                        Offset: FindFragmentOffset(first.PdfFragment)));

                matches.AddRange(found.Matches.Select(hit => new DocumentFindHit(hit.Text,
                    new DocumentFindSelection(hit.Position, Translation: true,
                        PhysicalPage: hit.Page, SentenceId: hit.SentenceId,
                        CachedMachine: CachedFindMachine(hit),
                        PdfFragment: hit.PdfFragment, SavedText: hit.Text,
                        SourceText: hit.SourceText))));
                if (found.Next is not { } next || next == cursor) return new(matches.ToArray(), null);
                space = DocumentFind.PageSize - matches.Count;
                cursor = next;
            }
        }
#else
        static Task<DocumentFindPage> AppendDetachedTranslationsAsync(
            List<DocumentFindHit> matches, string query, DocumentFindCursor start, string language, CancellationToken token) =>
            Task.FromResult(new DocumentFindPage(matches.ToArray(), null));
#endif

        async Task<DocumentFindPage> FindTranslatedAsync(string query, string language, DocumentFindCursor start, CancellationToken token)
        {
            var matches = new List<DocumentFindHit>(DocumentFind.PageSize);
            if (start.PhysicalPage is null)
            {
                var position = start.Position;
                var offset = start.Offset;
                while (true)
                {
                    token.ThrowIfCancellationRequested();
                    var found = await session.FindTranslationPageAsync(document.Id, language, query, position, token, offset);
                    if (!MatchesFindSource(found, document))
                        throw new InvalidOperationException("Текст документа змінився. Відкрийте пошук знову.");
                    matches.AddRange(found.Matches.Where(hit => hit.Position >= 0 && hit.Position < document.Sentences.Length)
                        .Select(hit => new DocumentFindHit(hit.Text, new DocumentFindSelection(hit.Position, Translation: true,
                            PdfFragment: hit.PdfFragment, SavedText: hit.Text, Alignment: hit.Alignment,
                            PivotLanguage: hit.PivotLanguage, SourceText: hit.SourceText))));
                    var nextOffset = found.NextOffset ?? 0;
                    if (found.NextStart is not int nextPosition || nextPosition >= document.Sentences.Length ||
                        nextPosition < position || (nextPosition == position && nextOffset <= offset))
                        break;
                    if (matches.Count > 0)
                        return new(matches.ToArray(), new DocumentFindCursor(nextPosition, Offset: nextOffset));
                    position = nextPosition;
                    offset = nextOffset;
                }
            }
            return await AppendDetachedTranslationsAsync(matches, query, start, language, token);
        }
    }
}
