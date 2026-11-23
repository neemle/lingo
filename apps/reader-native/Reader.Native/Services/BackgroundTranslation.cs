using Reader.Contracts;
using Reader.Presentation;
using System.ComponentModel;

namespace Reader.Native.Services;

internal sealed class BackgroundTranslation
{
    private static readonly TimeSpan Cadence = TimeSpan.FromSeconds(2);
    private readonly ReaderSession session;
    private readonly IDispatcher dispatcher;
    private readonly IDispatcherTimer timer;
    private readonly Dictionary<JobKey, BackgroundTranslationProgress> jobs = [];
    private readonly Dictionary<JobKey, LibraryFailure> libraryFailures = [];
    private Task pending = Task.CompletedTask;
    private CancellationTokenSource? stepCancellation;
    private bool prefetchingVisiblePage;
    private Document[] observedDocuments = [];
    private int wakeVersion;
    private DateTimeOffset retryAt;
    private bool enabled;
    private bool paused;
    private JobKey? lastLibraryJob;
    private bool libraryStepInvalidated;
    private (BackgroundTranslatedPage Address, string? DetachedText)? visiblePdf;
    private ((BackgroundTranslatedPage Address, string? DetachedText) Page, long RetryAt)? visibleAttempt;
    internal string Status { get; private set; } = "";
    internal bool Running { get; private set; }
    internal IReadOnlyList<TranslationPair> RequiredPackages { get; private set; } = [];
    internal int ParkedDocuments => libraryFailures.Values.Count(failure => failure.RetryAt is null);
    internal event Action? StatusChanged;

    private sealed record JobKey(string Document, string Revision, string Target);
    private sealed record LibraryFailure(int Attempts, long? RetryAt);

    internal BackgroundTranslation(ReaderSession session, IDispatcher dispatcher)
    {
        this.session = session;
        this.dispatcher = dispatcher;
        timer = dispatcher.CreateTimer();
        timer.Interval = Cadence;
        timer.Tick += Tick;
        session.PdfOcrUpdated += OnPdfOcrUpdated;
        session.PropertyChanged += OnSessionChanged;
        SetEnabled(session.GetReaderPreferences().BackgroundTranslation == true);
    }

    internal void SetVisiblePdf(BackgroundTranslatedPage? address, string? detachedText = null)
    {
        (BackgroundTranslatedPage Address, string? DetachedText)? next = address is null ? null : (address, detachedText);
        if (visiblePdf == next) return;
        visiblePdf = next;
        if (prefetchingVisiblePage)
            visibleAttempt = null;
        if (prefetchingVisiblePage || next is not null)
        {
            retryAt = default;
            stepCancellation?.Cancel();
        }
        Wake();
    }

    internal void SetEnabled(bool value)
    {
        enabled = value;
        visibleAttempt = null;
        if (!value)
        {
            stepCancellation?.Cancel();
            jobs.Clear();
            libraryFailures.Clear();
            lastLibraryJob = null;
            UpdateRequiredPackages();
        }
        if (enabled) Wake();
        else timer.Stop();
        SetStatus(value ? "Перевіряємо чергу перекладу…" : "");
    }

    internal async Task PauseAsync()
    {
        paused = true;
        timer.Stop();
        stepCancellation?.Cancel();
        if (enabled) SetStatus("Фоновий переклад призупинено.");
        await pending;
    }

    internal void Resume(bool clearProgress = false)
    {
        if (clearProgress) TranslationRoutesChanged();
        paused = false;
        Wake();
        if (enabled) SetStatus("Перевіряємо чергу перекладу…");
    }

    internal void TranslationRoutesChanged()
    {
        stepCancellation?.Cancel();
        jobs.Clear();
        libraryFailures.Clear();
        libraryStepInvalidated = true;
        lastLibraryJob = null;
        retryAt = default;
        visibleAttempt = null;
        UpdateRequiredPackages();
        StatusChanged?.Invoke();
        Wake();
    }

    internal void RetryParkedDocuments()
    {
        foreach (var key in libraryFailures.Where(entry => entry.Value.RetryAt is null).Select(entry => entry.Key).ToArray())
            libraryFailures.Remove(key);
        StatusChanged?.Invoke();
        Wake();
    }

    internal void TranslationReviewRemoved(string document, string target)
    {
        if (!enabled) return;
        foreach (var key in jobs.Keys.Where(key => key.Document == document && key.Target == target).ToArray())
            jobs.Remove(key);
        if (lastLibraryJob is { } active && active.Document == document && active.Target == target)
            libraryStepInvalidated = true;
        if (visibleAttempt is { Page.Address: var address } && address.DocumentId == document && address.Target == target)
            visibleAttempt = null;
        UpdateRequiredPackages();
        Wake();
    }

    internal void Stop()
    {
        SetEnabled(false);
        timer.Tick -= Tick;
        session.PdfOcrUpdated -= OnPdfOcrUpdated;
        session.PropertyChanged -= OnSessionChanged;
    }

    private void OnPdfOcrUpdated(string documentId, int page)
    {
        if (dispatcher.IsDispatchRequired)
        {
            dispatcher.Dispatch(() => OnPdfOcrUpdated(documentId, page));
            return;
        }
        if (!enabled) return;
        foreach (var key in jobs.Keys.Where(key => key.Document == documentId).ToArray()) jobs.Remove(key);
        if (lastLibraryJob?.Document == documentId) libraryStepInvalidated = true;
        // A neighbouring page can supply the rest of a sentence crossing the page break.
        if (visibleAttempt is { Page.Address: var address } && address.DocumentId == documentId &&
            Math.Abs(address.Page - page) <= 1)
            visibleAttempt = null;
        UpdateRequiredPackages();
        Wake();
    }

    internal void Wake()
    {
        wakeVersion++;
        Schedule(Cadence);
    }

    private void OnSessionChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (!enabled || args.PropertyName != nameof(ReaderSession.Snapshot)) return;
        if (dispatcher.IsDispatchRequired)
        {
            dispatcher.Dispatch(() => OnSessionChanged(sender, args));
            return;
        }
        var documents = session.Snapshot.Documents;
        if (ReferenceEquals(documents, observedDocuments)) return;
        var changed = documents.Length != observedDocuments.Length || documents.Zip(observedDocuments)
            .Any(pair => pair.First.Id != pair.Second.Id || pair.First.Language != pair.Second.Language ||
                pair.First.CurrentRevisionId != pair.Second.CurrentRevisionId);
        observedDocuments = documents;
        if (changed) Wake();
    }

    private void Schedule(TimeSpan delay)
    {
        if (!enabled || paused) return;
        if (timer.IsRunning && timer.Interval == delay) return;
        timer.Stop();
        timer.Interval = delay;
        timer.Start();
    }

    private void SetStatus(string text, bool running = false)
    {
        if (Status == text && Running == running) return;
        Status = text;
        Running = running;
        StatusChanged?.Invoke();
    }

    private void Tick(object? sender, EventArgs args)
    {
        if (!enabled || paused || session.Busy || !pending.IsCompleted || retryAt > DateTimeOffset.UtcNow)
            return;
        if (session.Snapshot.Documents.Length == 0)
        {
            timer.Stop();
            jobs.Clear();
            libraryFailures.Clear();
            lastLibraryJob = null;
            UpdateRequiredPackages();
            SetStatus("У бібліотеці ще немає документів.");
            StatusChanged?.Invoke();
        }
        else pending = RunStepAsync();
    }

    private void UpdateRequiredPackages()
    {
        var current = session.Document?.Id;
        var packages = jobs.OrderByDescending(job => job.Key.Document == current)
            .Select(job => job.Value.MissingPackages)
            .FirstOrDefault(packages => packages is { Length: > 0 }) ?? [];
        if (RequiredPackages.SequenceEqual(packages)) return;
        RequiredPackages = packages;
        StatusChanged?.Invoke();
    }

    private static string PairName(string source, string target) =>
        $"{DocumentLanguages.Name(source)} → {DocumentLanguages.Name(target)}";

    private async Task RunStepAsync()
    {
        using var cancellation = new CancellationTokenSource();
        using var libraryTimeout = CancellationTokenSource.CreateLinkedTokenSource(cancellation.Token);
        stepCancellation = cancellation;
        var token = cancellation.Token;
        JobKey? attemptedJob = null;
        string? attemptedTitle = null;
        try
        {
            var wake = wakeVersion;
            var preferences = await Task.Run(session.GetReaderPreferences);
            token.ThrowIfCancellationRequested();
            if (!enabled || paused || session.Busy) return;
            var current = session.Document?.Id;
            var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
            var live = new HashSet<JobKey>();
            var ready = new List<(JobKey Key, Document Document)>();
            string? missing = null;
            string? waitingForScan = null;
            var waiting = false;
            long? retry = null;
            foreach (var document in session.Snapshot.Documents.OrderByDescending(doc => doc.Id == current))
            {
                if (document.CurrentRevisionId is not { } revision) continue;
                var target = preferences.TranslationTargets.GetValueOrDefault(document.Language)
                    ?? (document.Language == "uk" ? "fr" : "uk");
                if (target == document.Language) continue;
                var key = new JobKey(document.Id, revision, target);
                live.Add(key);
                if (libraryFailures.TryGetValue(key, out var failure))
                {
                    if (failure.RetryAt is not long retryAt) continue;
                    if (retryAt > now)
                    {
                        waiting = true;
                        if (retry is null || retryAt < retry) retry = retryAt;
                        continue;
                    }
                }
                if (!jobs.TryGetValue(key, out var job) || job.ResumeAt is { } deadline && deadline <= now)
                    ready.Add((key, document));
                else if (job.MissingPackages is { } packages)
                    missing ??= packages.Length == 0
                        ? $"Офлайн-маршрут недоступний · {PairName(document.Language, target)}."
                        : $"Потрібні мовні пакети · {string.Join(", ", packages.Select(pair => PairName(pair.Source, pair.Target)))}.";
                else if (job.WaitingForPdfOcr)
                    waitingForScan ??= document.Title;
                else if (job.ResumeAt is not null)
                    waiting = true;
                if (job?.ResumeAt is { } due && due > now && (retry is null || due < retry))
                    retry = due;
            }
            foreach (var key in jobs.Keys.Where(key => !live.Contains(key)).ToArray()) jobs.Remove(key);
            var previouslyParked = ParkedDocuments;
            foreach (var key in libraryFailures.Keys.Where(key => !live.Contains(key)).ToArray()) libraryFailures.Remove(key);
            if (ParkedDocuments != previouslyParked) StatusChanged?.Invoke();
            UpdateRequiredPackages();
            if (visiblePdf is { } visible && current == visible.Address.DocumentId &&
                (visibleAttempt is not { } attempted || attempted.Page != visible || attempted.RetryAt <= now))
            {
                var address = visible.Address;
                var key = new JobKey(address.DocumentId, address.RevisionId, address.Target);
                if (live.Contains(key) && jobs.GetValueOrDefault(key)?.MissingPackages is null)
                {
                    prefetchingVisiblePage = true;
                    Schedule(Cadence);
                    SetStatus($"Опрацьовуємо відкриту сторінку {address.Page + 1}…", running: true);
                    visibleAttempt = (visible, now + 30);
                    var complete = await session.PrefetchVisiblePdfTranslationAsync(address, token);
                    token.ThrowIfCancellationRequested();
                    if (enabled && !paused && visiblePdf == visible && visibleAttempt == (visible, now + 30))
                    {
                        visibleAttempt = (visible, complete ? long.MaxValue : DateTimeOffset.UtcNow.ToUnixTimeSeconds() + 30);
                        SetStatus(complete ? $"Переклад сторінки {address.Page + 1} у кеші."
                            : "Черга перекладу продовжується.");
                    }
                    return;
                }
            }
            if (ready.Count == 0)
            {
                SetStatus(missing ?? (waiting ? "Очікує на повторну спробу."
                    : waitingForScan is not null ? $"Відкрийте сторінки «{waitingForScan}», щоб розпізнати текст для перекладу."
                    : ParkedDocuments > 0 ? "Фоновий переклад призупинено для окремих документів."
                    : "Чергу перекладу опрацьовано."));
                if (visibleAttempt is { RetryAt: var visibleRetry } attempt && attempt.Page == visiblePdf &&
                    visibleRetry > now && visibleRetry < long.MaxValue && (retry is null || visibleRetry < retry))
                    retry = visibleRetry;
                if (wake != wakeVersion) return;
                if (retry is long due) Schedule(TimeSpan.FromSeconds(Math.Max(Cadence.TotalSeconds, due - now)));
                else timer.Stop();
                return;
            }
            Schedule(Cadence);

            var previous = ready.FindIndex(candidate => candidate.Key == lastLibraryJob);
            var work = ready[(previous + 1) % ready.Count];
            var title = work.Document.Title;
            attemptedJob = work.Key;
            attemptedTitle = title;
            lastLibraryJob = work.Key;
            libraryStepInvalidated = false;
            libraryTimeout.CancelAfter(TimeSpan.FromMinutes(2));
            token = libraryTimeout.Token;
            SetStatus($"Опрацьовуємо «{title}»…", running: true);
            var progress = await session.AdvanceBackgroundTranslationAsync(work.Key.Document, work.Key.Revision, work.Key.Target, token);
            token.ThrowIfCancellationRequested();
            if (!enabled || paused || libraryStepInvalidated) return;
            jobs[work.Key] = progress;
            UpdateRequiredPackages();
#if MACCATALYST
            if (progress.PendingPdfTextPage is int pendingPage)
            {
                var pageCount = work.Document.PdfPageStarts?.Length ?? work.Document.OcrPdfPages
                    ?? throw new InvalidDataException("Кількість сторінок PDF невідома.");
                var original = await session.OriginalFileAsync(work.Key.Document)
                    ?? throw new InvalidDataException("Оригінал PDF недоступний.");
                if (!enabled || paused || libraryStepInvalidated) return;
                token.ThrowIfCancellationRequested();
                SetStatus($"Читаємо текстовий шар сторінки {pendingPage + 1} · «{title}»…", running: true);
                var text = await PdfScan.ReadPageTextAsync(original.Path, pendingPage, pageCount, token);
                token.ThrowIfCancellationRequested();
                if (!enabled || paused || libraryStepInvalidated) return;
                await session.CacheBackgroundPdfTextLayerAsync(work.Key.Document, work.Key.Revision, pendingPage, text);
                token.ThrowIfCancellationRequested();
                libraryFailures.Remove(work.Key);
                if (enabled && !paused) SetStatus("Черга перекладу продовжується.");
                return;
            }
#endif
            libraryFailures.Remove(work.Key);
            SetStatus(progress.MissingPackages is not null
                ? $"Для «{title}» бракує офлайн-маршруту."
                : progress.TranslatedPage is int page
                    ? work.Document.Format == "pdf"
                        ? $"Переклад сторінки {page + 1} · «{title}»."
                        : $"Переклад фото «{title}»."
                    : $"Перевірено кеш · «{title}».");
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            if (prefetchingVisiblePage) visibleAttempt = null;
            if (enabled && !paused) SetStatus("Перевіряємо чергу перекладу…");
        }
        catch (Exception error)
        {
            if (!enabled || paused || cancellation.IsCancellationRequested) return;
            if (attemptedJob is { } key)
            {
                if (libraryStepInvalidated) return;
                var attempts = (libraryFailures.GetValueOrDefault(key)?.Attempts ?? 0) + 1;
                long? due = attempts < 3 ? DateTimeOffset.UtcNow.ToUnixTimeSeconds() + attempts * 20 : null;
                libraryFailures[key] = new(attempts, due);
                jobs.Remove(key);
                System.Diagnostics.Trace.TraceWarning("Background translation job {0}, attempt {1}, parked={2}, timeout={3}, error={4}",
                    key.Document, attempts, due is null, libraryTimeout.IsCancellationRequested, error.GetType().Name);
                SetStatus(due is null ? $"Переклад «{attemptedTitle}» призупинено."
                    : $"Переклад «{attemptedTitle}» повторить спробу.");
                StatusChanged?.Invoke();
            }
            else
            {
                System.Diagnostics.Debug.WriteLine(error);
                retryAt = DateTimeOffset.UtcNow.AddSeconds(10);
                Schedule(TimeSpan.FromSeconds(10));
                SetStatus("Фоновий переклад повторить спробу.");
            }
        }
        finally
        {
            prefetchingVisiblePage = false;
            stepCancellation = null;
        }
    }
}
