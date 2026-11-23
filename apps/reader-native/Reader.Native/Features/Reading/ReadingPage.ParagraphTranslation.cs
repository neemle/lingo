using Reader.Contracts;
using Reader.CoreClient;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private const int TranslationWindowLimit = 6;
    private readonly List<TranslationWindow> translationWindows = [];
    private bool readingPageActive;
    private bool autoTranslate;
    private PendingTranslation? scheduledTranslation;
    private TranslationKey? lastAutoTranslation;
    private readonly Queue<(TranslationKey Key, string? Pivot)> prefetchAttempts = new();
    private TranslationKey? lastPrefetchOrigin;
    private bool prefetching;
    private TranslationKey? translationScrollKey;
    private bool translationScrollPending;
    private bool translationScrollQueued;
    private readonly List<PreviewRow> paragraphPreviewRows = [];
    private readonly List<PreviewRow> detachedPreviewRows = [];
    private int translationViewportRequest;
    private long translationScrollSuppressedUntil;
    private bool sourceViewportSyncQueued;
    private long lastSourceViewportMovement;
    private bool selectingPreviewRow;
    private bool restoringTranslationCache;
    private CachedWindowRequest? lastCacheRestore;

    private sealed record PreviewRow(int Position, VisualElement View);

    private sealed record TranslationKey(string Document, string Sentence, string Source, string Target, string Text);
    private sealed record TranslationScope(string Document, string Source, string Target);
    private sealed record CachedWindowRequest(TranslationKey Key, string? Revision, int ReadinessRequest);
    private sealed record PrefetchCandidate(int Position, TranslationKey Key, string? Pivot);
    private sealed record AutoTranslationRequest(TranslationKey Active, PrefetchCandidate? Lookahead = null);
    private sealed record PendingTranslation(AutoTranslationRequest Request, CancellationTokenSource Cancellation);

    private void InlinePdfViewportScrolled(object? sender, ScrolledEventArgs args)
    {
        if (!showInlineOriginal) return;
        if (ReferenceEquals(sender, ActiveInlinePdf().Viewport))
        {
            PositionWordPopup();
            if (followingInlinePdf == 0) _ = FollowPdfTranslationScrollAsync(fromOriginal: true);
        }
        ScheduleSourceViewportSync();
    }

    private async void ScheduleSourceViewportSync()
    {
        if (!CanSyncSourceViewport()) return;
        lastSourceViewportMovement = Environment.TickCount64;
        if (sourceViewportSyncQueued) return;
        sourceViewportSyncQueued = true;
        try
        {
            while (Environment.TickCount64 - lastSourceViewportMovement < 220)
                await Task.Delay(220);
            if (!CanSyncSourceViewport()) return;
            var detached = DetachedPdfReader.IsVisible;
            var source = detached ? DetachedPdfText : SourceText;
            var visible = showInlineOriginal ? VisibleInlinePdfOffsets() : source.VisibleOffsets?.Invoke();
            if (detached)
            {
                if (detachedAddress is { } address && detachedSentence?.Sentence is { } current &&
                    visible is { } page && (page.Start >= current.EndUtf16 || page.End <= current.StartUtf16))
                    await LoadDetachedSentenceAsync(address, position: null, sourceOffset: page.Start);
                return;
            }

            var session = AppState.Session;
            if (session.Document is not { } document || session.Sentence is not { } active) return;
            if (visible is not { } range || (range.Start < active.EndUtf16 && range.End > active.StartUtf16)) return;
            var position = Array.FindIndex(document.Sentences,
                sentence => sentence.EndUtf16 > range.Start && sentence.StartUtf16 < range.End);
            if (position >= 0) await session.SelectSentenceAsync(position);
        }
        finally { sourceViewportSyncQueued = false; }
    }

    private bool CanSyncSourceViewport() => readingPageActive && TranslationVisible && SourcePane.IsVisible &&
        !WordOverlay.IsVisible && !utteranceActive && !AppState.Session.Busy &&
        (DetachedPdfReader.IsVisible ? !loadingDetachedSentence && !speakingDetachedSentence : !PdfPageNotice.IsVisible);

    private void SuppressTranslationViewportSync()
    {
        translationViewportRequest++;
        translationScrollSuppressedUntil = Environment.TickCount64 + 280;
    }

    private async void TranslationViewportScrolled(object? sender, ScrolledEventArgs args)
    {
        var detached = ReferenceEquals(sender, DetachedTranslationViewport);
        var viewport = detached ? DetachedTranslationViewport : TranslationViewport;
        bool Current() => CanSyncSourceViewport() && !selectingPreviewRow && !translating &&
            !translatingDetachedSentence && detached == DetachedPdfReader.IsVisible &&
            Environment.TickCount64 >= translationScrollSuppressedUntil && viewport.IsVisible;
        if (!Current()) return;

        var request = ++translationViewportRequest;
        await Task.Delay(220);
        if (request != translationViewportRequest || !Current()) return;
        var top = viewport.ScrollY;
        var bottom = top + viewport.Height;
        var active = detached ? DetachedTranslationPane : ActiveTranslationCard;
        if (!active.IsVisible || active.Height <= 0 || (active.Y < bottom && active.Y + active.Height > top)) return;
        var rows = detached ? detachedPreviewRows : paragraphPreviewRows;
        var visible = rows.Where(row => row.View is { Height: > 0, Parent: VisualElement parent } &&
            parent.Y + row.View.Y < bottom && parent.Y + row.View.Y + row.View.Height > top);
        var row = active.Y >= bottom ? visible.LastOrDefault() : visible.FirstOrDefault();
        if (row is null) return;

        selectingPreviewRow = true;
        try
        {
            if (detached && detachedAddress is { } address && detachedSentence?.Position != row.Position)
                await LoadDetachedSentenceAsync(address, row.Position);
            else if (!detached && AppState.Session.Document?.Position != row.Position)
                await AppState.Session.SelectSentenceAsync(row.Position);
        }
        finally { selectingPreviewRow = false; }
    }

    private void TrackTranslationScroll(TranslationKey? key)
    {
        if (translationScrollKey != key)
        {
            translationScrollKey = key;
            RequestTranslationScroll();
        }
        else QueueTranslationScroll();
    }

    private void RequestTranslationScroll()
    {
        translationScrollPending = true;
        QueueTranslationScroll();
    }

    private void TranslationViewportReady(object? sender, EventArgs args) => QueueTranslationScroll();

    private bool CanScrollTranslation() => readingPageActive && TranslationVisible &&
        TranslationViewport.Handler is not null && TranslationViewport.Width > 0 && TranslationViewport.Height > 0;

    private void QueueTranslationScroll()
    {
        if (!translationScrollPending || translationScrollQueued || !CanScrollTranslation()) return;
        translationScrollQueued = true;
        if (!Dispatcher.Dispatch(ScrollToActiveTranslation)) translationScrollQueued = false;
    }

    private async void ScrollToActiveTranslation()
    {
        translationScrollQueued = false;
        if (!translationScrollPending || !CanScrollTranslation()) return;
        translationScrollPending = false;
        try
        {
            var top = TranslationViewport.ScrollY;
            var bottom = top + TranslationViewport.Height;
            if (!ActiveTranslationCard.IsVisible)
            {
                if (top > 0)
                {
                    SuppressTranslationViewportSync();
                    await TranslationViewport.ScrollToAsync(0, 0, animated: false);
                }
            }
            else if (ActiveTranslationCard.Y < top || ActiveTranslationCard.Y + ActiveTranslationCard.Height > bottom)
            {
                SuppressTranslationViewportSync();
                await TranslationViewport.ScrollToAsync(ActiveTranslationCard, ScrollToPosition.MakeVisible, animated: false);
            }
        }
        catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
    }

    private void UpdateAutoTranslateButton()
    {
        AutoTranslate.Text = DetachedAutoTranslate.Text = autoTranslate ? "Автопереклад ✓" : "Автопереклад";
        SemanticProperties.SetDescription(AutoTranslate, autoTranslate
            ? "Вимкнути автоматичний офлайн-переклад абзаців"
            : "Увімкнути автоматичний офлайн-переклад абзаців");
    }

    private void AutoTranslateClicked(object? sender, EventArgs args)
    {
        autoTranslate = !autoTranslate;
        lastAutoTranslation = null;
        pdfAutoAttempts = null;
        SaveReaderPreference(ReaderPreferenceChange.AutoTranslate(autoTranslate));
        UpdateAutoTranslateButton();
        RefreshPdfTranslationLayout();
        ScheduleAutoTranslation();
        if (autoTranslate) _ = RestoreDetachedTranslationAsync();
    }

    private void BackgroundTextTranslated(BackgroundTranslatedText translated)
    {
        var session = AppState.Session;
        if (!readingPageActive || session.Document is not { } document ||
            document.Id != translated.DocumentId || document.CurrentRevisionId != translated.RevisionId ||
            TargetCode != translated.Target || session.Sentence is not { } sentence)
            return;

        var window = FindTranslationWindow(document, translated.Target);
        if (!translated.SentenceIds.Contains(sentence.Id) &&
            window?.Units.Any(unit => unit.Sources.Any(source => translated.SentenceIds.Contains(source.Id))) != true)
            return;

        lastCacheRestore = null;
        _ = RestoreCachedTranslationAsync();
    }

    private async Task RestoreCachedTranslationAsync()
    {
        var session = AppState.Session;
        if (!readingPageActive || restoringTranslationCache || translating || session.Busy ||
            (readiness is null && readinessError is null) || ActiveTranslationKey() is not { } key ||
            readinessPair != (key.Source, key.Target) || session.Document is not { } document)
            return;

        var request = new CachedWindowRequest(key, document.CurrentRevisionId, readinessRequest);
        if (lastCacheRestore == request) return;
        if (FindTranslationWindow(document, key.Target) is { IsParagraph: true } existing && IsPreferredRoute(existing) &&
            !HasUntranslatedUnits(document, existing) &&
            (ActiveMachineAlignment(document, key.Target, preferredOnly: true) is not null ||
                ReviewedFor(document, key.Target, key.Sentence) is not null))
            return;

        lastCacheRestore = request;
        restoringTranslationCache = true;
        try
        {
            var cached = await session.ReadCachedTranslationAsync(key.Document, key.Sentence, key.Target);
            if (!readingPageActive || translating || ActiveTranslationKey() != key ||
                request.ReadinessRequest != readinessRequest ||
                session.Document is not { } current || current.Position != document.Position ||
                current.CurrentRevisionId != request.Revision || current.Text != document.Text)
                return;

            var scope = new TranslationScope(key.Document, key.Source, key.Target);
            var window = BuildParagraphWindow(current, scope, cached.Paragraph);
            var matching = MatchingTranslationWindows(current, key.Target).ToArray();
            var previous = matching.FirstOrDefault(candidate => candidate.IsParagraph && candidate.Anchor == window.Anchor
                && candidate.Pivot == window.Pivot);
            if (window.Units.Any(unit => MachineFor(current, window, unit) is not null &&
                (previous?.UnitFor(unit.First) is not { } old || MachineFor(current, previous, old) is null)))
            {
                if (previous is not null)
                    window = window with { Units = window.Units.Select(unit =>
                        unit.MachineText is null && previous.UnitFor(unit.First) is { } old &&
                        unit.Sources.SequenceEqual(old.Sources) && MachineFor(current, previous, old) is not null
                            ? old : unit).ToArray() };
                RememberWindow(window);
            }

            if (cached.Sentence is { } sentence && ReviewedFor(current, key.Target, key.Sentence) is null &&
                !matching.Any(previous => previous.Pivot == sentence.PivotLanguage
                    && previous.UnitFor(current.Position) is { } unit && MachineFor(current, previous, unit) is not null))
                RememberWindow(BuildSentenceWindow(current, scope, sentence), background: true);
        }
        catch (CoreException) { } // Cache lookup can race a library change. Manual translation stays available.
        finally
        {
            restoringTranslationCache = false;
            if (readingPageActive) UpdateTranslation();
        }
    }

    private TranslationKey? PendingAutoTranslation() =>
        readingPageActive && autoTranslate && TranslationVisible && TranslateOffline.IsEnabled
            ? ActiveTranslationKey() : null;

    private void CancelScheduledTranslation()
    {
        var pending = scheduledTranslation;
        scheduledTranslation = null;
        pending?.Cancellation.Cancel();
    }

    private void ScheduleAutoTranslation()
    {
        var request = NextTranslationRequest();
        if (scheduledTranslation?.Request == request) return;
        CancelScheduledTranslation();
        if (request is not null) _ = TranslateAfterNavigationAsync(request);
    }

    private AutoTranslationRequest? NextTranslationRequest()
    {
        if (restoringTranslationCache || PdfPageNotice.IsVisible || PdfUsesPageTranslation) return null;
        if (PendingAutoTranslation() is { } foreground && foreground != lastAutoTranslation) return new(foreground);
        if (!readingPageActive || !autoTranslate || !TranslationVisible || translating || utteranceActive || prefetching ||
            readiness is not { Ready: true } route || ActiveTranslationKey() is not { } active || active == lastPrefetchOrigin ||
            readinessPair != (active.Source, active.Target)) return null;

        var session = AppState.Session;
        if (session.Busy || session.Document is not { } document ||
            FindTranslationWindow(document, active.Target) is not { IsParagraph: true } window || !IsPreferredRoute(window) ||
            (ReviewedFor(document, active.Target, active.Sentence) is null &&
                ActiveMachineAlignment(document, active.Target, preferredOnly: true) is null))
            return null;

        for (var index = document.Position + 1; index < Math.Min(document.Sentences.Length, document.Position + 3); index++)
        {
            var sentence = document.Sentences[index];
            if (ReviewedFor(document, active.Target, sentence.Id) is not null ||
                HasMachineAt(document, active.Target, index)) continue;

            var key = active with { Sentence = sentence.Id, Text = sentence.Text };
            return prefetchAttempts.Contains((key, route.PivotLanguage)) ? null
                : new AutoTranslationRequest(active, new PrefetchCandidate(index, key, route.PivotLanguage));
        }
        return null;
    }

    private async Task TranslateAfterNavigationAsync(AutoTranslationRequest request)
    {
        using var cancellation = new CancellationTokenSource();
        var pending = new PendingTranslation(request, cancellation);
        scheduledTranslation = pending;
        try { await Task.Delay(request.Lookahead is null ? 350 : 900, cancellation.Token); }
        catch (OperationCanceledException) { return; }
        if (!ReferenceEquals(scheduledTranslation, pending)) return;
        scheduledTranslation = null;
        if (NextTranslationRequest() != request)
        {
            ScheduleAutoTranslation();
            return;
        }
        if (request.Lookahead is { } lookahead)
        {
            await PrefetchTranslationAsync(request.Active, lookahead);
            return;
        }
        lastAutoTranslation = request.Active;
        await TranslateOfflineAsync(useContext: true);
    }

    private async Task PrefetchTranslationAsync(TranslationKey origin, PrefetchCandidate candidate)
    {
        var session = AppState.Session;
        if (session.Document is not { CurrentRevisionId: { } revision } before) return;
        var key = candidate.Key;
        prefetching = true;
        lastPrefetchOrigin = origin;
        try
        {
            var result = await session.PrefetchTranslationWindowAsync(key.Document, key.Sentence, revision, key.Target);
            if (result is null) return;
            if (prefetchAttempts.Count >= 2 * TranslationWindowLimit) prefetchAttempts.Dequeue();
            prefetchAttempts.Enqueue((key, candidate.Pivot));
            if (!readingPageActive || !autoTranslate || !TranslationVisible || ActiveTranslationKey() != origin ||
                TargetCode != key.Target ||
                readiness is not { Ready: true } route || readinessPair != (key.Source, key.Target) ||
                route.PivotLanguage != candidate.Pivot || result.PivotLanguage != candidate.Pivot ||
                session.Document is not { } current || current.Id != key.Document || current.Language != key.Source ||
                current.CurrentRevisionId != revision || current.Text != before.Text ||
                current.Sentences.ElementAtOrDefault(candidate.Position)?.Id != key.Sentence ||
                HasMachineAt(current, key.Target, candidate.Position)) return;

            var scope = new TranslationScope(key.Document, key.Source, key.Target);
            var window = BuildParagraphWindow(current with { Position = candidate.Position }, scope, result);
            if (window.UnitFor(candidate.Position) is not { } next || MachineFor(current, window, next) is null) return;
            RememberWindow(window, background: true);
            if (!translating && window.UnitFor(current.Position) is not null) UpdateTranslation();
        }
        catch (Exception) { }
        finally
        {
            prefetching = false;
            if (readingPageActive && ActiveTranslationKey() != origin) ScheduleAutoTranslation();
        }
    }

    private sealed record AlignedUnit(int First, Sentence[] Sources, string? MachineText, bool JoinedLines,
        string? SourceSpan, string? Pivot)
    {
        public bool Contains(int index) => index >= First && index < First + Sources.Length;

        public bool Matches(Document document)
        {
            if (First + Sources.Length > document.Sentences.Length) return false;
            for (var i = 0; i < Sources.Length; i++)
                if (Sources[i] != document.Sentences[First + i]) return false;

            return SourceSpan is null || document.Text.AsSpan(Sources[0].StartUtf16, SourceSpan.Length)
                .SequenceEqual(SourceSpan);
        }
    }

    private sealed record TranslationWindow(TranslationScope Scope, int Anchor, AlignedUnit[] Units,
        string? Pivot, bool IsParagraph, bool MoreBefore, bool MoreAfter)
    {
        public AlignedUnit? UnitFor(int position) => Array.Find(Units, unit => unit.Contains(position));
    }

    private bool IsPreferredRoute(TranslationWindow window) =>
        IsPreferredRoute(window, AppState.Session.Document?.Position ?? window.Anchor);

    private bool IsPreferredRoute(TranslationWindow window, int position) =>
        readiness is { Ready: true } current && readinessPair == (window.Scope.Source, window.Scope.Target)
            && (window.UnitFor(position) is { MachineText: not null } unit ? unit.Pivot : window.Pivot)
                == current.PivotLanguage;

    private IEnumerable<TranslationWindow> MatchingTranslationWindows(Document document, string target, int? position = null)
    {
        var scope = new TranslationScope(document.Id, document.Language, target);
        var index = position ?? document.Position;
        return translationWindows.Where(window => window.Scope == scope
            && window.UnitFor(index) is { } unit && unit.Matches(document));
    }

    private TranslationWindow? FindTranslationWindow(Document document, string target) =>
        MatchingTranslationWindows(document, target).OrderByDescending(IsPreferredRoute)
            .ThenByDescending(window => window.IsParagraph).FirstOrDefault();

    private MachineTranslation? ActiveMachine(Document document, string target) =>
        ActiveMachineAlignment(document, target)?.Translation;

    private (TranslationWindow Window, AlignedUnit Unit, MachineTranslation Translation)? ActiveMachineAlignment(
        Document document, string target, bool preferredOnly = false)
    {
        foreach (var window in MatchingTranslationWindows(document, target).OrderByDescending(IsPreferredRoute))
            if ((!preferredOnly || IsPreferredRoute(window)) && window.UnitFor(document.Position) is { } unit
                && MachineFor(document, window, unit) is { } machine)
                return (window, unit, machine);
        return null;
    }

    private bool HasMachineAt(Document document, string target, int position) =>
        MatchingTranslationWindows(document, target, position).Any(window =>
            IsPreferredRoute(window, position) && window.UnitFor(position) is { } unit
                && MachineFor(document, window, unit) is not null);

    private static string? ReviewedFor(Document document, string target, string sentenceId) =>
        document.ReviewedTranslations.GetValueOrDefault(target)?.GetValueOrDefault(sentenceId);

    private static MachineTranslation? MachineFor(Document document, TranslationWindow window, AlignedUnit unit)
    {
        if (unit.MachineText is not { } text || !unit.Matches(document)
            || unit.Sources.Any(source => ReviewedFor(document, window.Scope.Target, source.Id) is not null))
            return null;
        return new MachineTranslation(text, unit.Pivot, unit.JoinedLines);
    }

    private bool HasUntranslatedUnits(Document document, TranslationWindow window) =>
        window.Units.Any(unit => unit.Matches(document)
            && unit.Sources.Any(source => ReviewedFor(document, window.Scope.Target, source.Id) is null)
            && MachineFor(document, window, unit) is null);

    private string? IndependentMachineText(Document document, string target) =>
        ActiveMachine(document, target) is { JoinedLines: false } machine ? machine.Text : null;

    private void RememberWindow(TranslationWindow window, bool background = false)
    {
        translationWindows.RemoveAll(previous => previous.Scope == window.Scope
            && previous.Anchor == window.Anchor && previous.IsParagraph == window.IsParagraph
            && previous.Pivot == window.Pivot);
        var index = background ? Math.Min(translationWindows.Count, TranslationWindowLimit - 1) : 0;
        translationWindows.Insert(index, window);
        if (translationWindows.Count > TranslationWindowLimit)
            translationWindows.RemoveAt(TranslationWindowLimit);
    }

    private static TranslationWindow BuildParagraphWindow(Document document, TranslationScope scope,
        ParagraphTranslation paragraph)
    {
        var ids = paragraph.Units.SelectMany(unit => unit.SentenceIds).ToHashSet(StringComparer.Ordinal);
        var indices = new Dictionary<string, int>(ids.Count, StringComparer.Ordinal);
        for (var i = 0; i < document.Sentences.Length; i++)
            if (ids.Contains(document.Sentences[i].Id)) indices.Add(document.Sentences[i].Id, i);

        var units = new List<AlignedUnit>(paragraph.Units.Length);
        foreach (var unit in paragraph.Units)
        {
            if (unit.SentenceIds.Length == 0 || !indices.TryGetValue(unit.SentenceIds[0], out var first)
                || unit.SentenceIds.Length > document.Sentences.Length - first)
                continue;
            var sources = document.Sentences.AsSpan(first, unit.SentenceIds.Length).ToArray();
            if (!sources.Select(source => source.Id).SequenceEqual(unit.SentenceIds, StringComparer.Ordinal))
                continue;
            var joined = sources.Length > 1;
            var span = joined ? document.Text[sources[0].StartUtf16..sources[^1].EndUtf16] : null;
            units.Add(new AlignedUnit(first, sources, unit.Reviewed ? null : unit.Translation, joined, span,
                unit.PivotLanguage));
        }
        return new TranslationWindow(scope, document.Position, units.ToArray(),
            paragraph.PivotLanguage, true, paragraph.MoreBefore, paragraph.MoreAfter);
    }

    private static TranslationWindow BuildSentenceWindow(Document document, TranslationScope scope,
        MachineTranslation sentence)
    {
        var unit = new AlignedUnit(document.Position, [document.Sentences[document.Position]],
            sentence.Text, sentence.JoinedLines, null, sentence.PivotLanguage);
        return new TranslationWindow(scope, document.Position, [unit], sentence.PivotLanguage, false, false, false);
    }

    private async void TranslateOfflineClicked(object? sender, EventArgs args)
    {
        lastAutoTranslation = ActiveTranslationKey();
        await TranslateOfflineAsync(true);
    }

    private async void TranslateSentenceClicked(object? sender, EventArgs args) => await TranslateOfflineAsync(false);

    private async Task TranslateOfflineAsync(bool useContext)
    {
        var session = AppState.Session;
        if (translating || session.Busy || readiness?.Ready != true || ActiveTranslationKey() is not { } key
            || session.Document is not { } document) return;

        translating = true;
        statusKey = key;
        machineStatus = useContext ? "Перекладаємо абзац…" : "Перекладаємо речення…";
        UpdateTranslation();
        var scope = new TranslationScope(key.Document, key.Source, key.Target);
        try
        {
            if (useContext)
            {
                try
                {
                    var result = await session.TranslateParagraphAsync(key.Document, key.Sentence, key.Target);
                    var paragraph = BuildParagraphWindow(document, scope, result);
                    if (paragraph.Units.Length > 0) RememberWindow(paragraph);
                    if (paragraph.UnitFor(document.Position) is { } translated && session.Document is { } current &&
                        MachineFor(current, paragraph, translated) is not null)
                    {
                        machineStatus = null;
                        return;
                    }
                }
                catch (Exception error)
                {
                    if (ActiveTranslationKey() != key || ReviewedFor(document, key.Target, key.Sentence) is not null)
                        throw;
                    machineStatus = error.Message;
                }
            }

            if (ActiveTranslationKey() != key) return;
            if (session.ReviewedTranslation(key.Target) is not null)
            {
                machineStatus = null;
                return;
            }

            var sentence = await session.TranslateSentenceAsync(key.Document, key.Sentence, key.Target);
            if (sentence is not null) RememberWindow(BuildSentenceWindow(document, scope, sentence));
            else readinessPair = null;
            machineStatus = null;
        }
        catch (Exception error) { machineStatus = error.Message; }
        finally
        {
            translating = false;
            UpdateTranslation();
        }
    }

    private void UpdateSourceLocation(Document? document, Sentence? sentence)
    {
        if (document is null || sentence is null)
        {
            SourceLocation.IsVisible = false;
            return;
        }

        var block = Array.Find(document.Blocks, value => value.StartUtf16 <= sentence.StartUtf16
            && sentence.StartUtf16 < value.EndUtf16);
        var location = block?.Kind == "heading" ? "Заголовок"
            : block?.Page is int page ? $"Сторінка {page + 1}"
            : block is not null ? "Абзац" : "";
        SourceLocation.Text = location;
        SourceLocation.IsVisible = location.Length > 0;
    }

    private void UpdateParagraphPreview(Document? document, TranslationWindow? window, AlignedUnit? active)
    {
        SuppressTranslationViewportSync();
        paragraphPreviewRows.Clear();
        ParagraphBefore.Children.Clear();
        ParagraphAfter.Children.Clear();
        ParagraphBefore.IsVisible = ParagraphAfter.IsVisible = false;
        if (document is null || window is not { IsParagraph: true }) return;

        foreach (var unit in window.Units.OrderBy(unit => unit.First))
        {
            if (!unit.Matches(document)) continue;
            if (unit != active && MachineFor(document, window, unit) is { } translated)
                AddPreviewRow(document, unit.First, unit.Sources.Length, translated.Text);
            else
                for (var i = 0; i < unit.Sources.Length; i++)
                    if (unit.First + i != document.Position &&
                        ReviewedFor(document, window.Scope.Target, unit.Sources[i].Id) is { } review)
                        AddPreviewRow(document, unit.First + i, 1, review);
        }
    }

    private void AddPreviewRow(Document document, int position, int count, string text)
    {
        var sentenceId = document.Sentences[position].Id;
        var label = new Label { Text = text, FontFamily = "Georgia", FontSize = Math.Max(16, SourceText.ReadingFontSize - 4),
            LineHeight = 1.4, TextColor = Color.FromArgb("#655F4D"), Margin = new Thickness(12, 0) };
        SemanticProperties.SetDescription(label, count == 1 ? $"Речення {position + 1}: {text}"
            : $"Речення {position + 1}–{position + count}: {text}");
        var tap = new TapGestureRecognizer();
        tap.Tapped += SelectRow;
        label.GestureRecognizers.Add(tap);
        var column = position < document.Position ? ParagraphBefore : ParagraphAfter;
        column.Children.Add(label);
        column.IsVisible = true;
        paragraphPreviewRows.Add(new(position, label));

        async void SelectRow(object? sender, EventArgs args)
        {
            if (selectingPreviewRow || !readingPageActive || !TranslationVisible) return;
            var session = AppState.Session;
            if (session.Busy || session.Document is not { } current || current.Id != document.Id
                || current.Position == position || current.Sentences.ElementAtOrDefault(position)?.Id != sentenceId) return;
            selectingPreviewRow = true;
            try
            {
                StopSpeech();
                await session.SelectSentenceAsync(position);
            }
            finally { selectingPreviewRow = false; }
        }
    }
}
