using Reader.Contracts;
using Reader.Presentation;

namespace Reader.Native.Services;

internal sealed class BackgroundTranslation
{
    private readonly ReaderSession session;
    private readonly IDispatcher dispatcher;
    private readonly IDispatcherTimer timer;
    private readonly Dictionary<JobKey, BackgroundTranslationProgress> jobs = [];
    private Task pending = Task.CompletedTask;
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
    internal event Action? StatusChanged;

    private sealed record JobKey(string Document, string Revision, string Target);

    internal BackgroundTranslation(ReaderSession session, IDispatcher dispatcher)
    {
        this.session = session;
        this.dispatcher = dispatcher;
        timer = dispatcher.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(2);
        timer.Tick += Tick;
        session.PdfOcrUpdated += OnPdfOcrUpdated;
        SetEnabled(session.GetReaderPreferences().BackgroundTranslation == true);
    }

    internal void SetVisiblePdf(BackgroundTranslatedPage? address, string? detachedText = null) =>
        visiblePdf = address is null ? null : (address, detachedText);

    internal void SetEnabled(bool value)
    {
        enabled = value;
        visibleAttempt = null;
        if (!value)
        {
            jobs.Clear();
            lastLibraryJob = null;
            UpdateRequiredPackages();
        }
        UpdateTimer();
        SetStatus(value ? "Перевіряємо чергу перекладу…" : "");
    }

    internal async Task PauseAsync()
    {
        paused = true;
        UpdateTimer();
        if (enabled) SetStatus("Фоновий переклад призупинено.");
        await pending;
    }

    internal void Resume(bool clearProgress = false)
    {
        if (clearProgress) TranslationRoutesChanged();
        paused = false;
        UpdateTimer();
        if (enabled) SetStatus("Перевіряємо чергу перекладу…");
    }

    internal void TranslationRoutesChanged()
    {
        jobs.Clear();
        libraryStepInvalidated = true;
        lastLibraryJob = null;
        retryAt = default;
        visibleAttempt = null;
        UpdateRequiredPackages();
    }

    internal void Stop()
    {
        SetEnabled(false);
        timer.Tick -= Tick;
        session.PdfOcrUpdated -= OnPdfOcrUpdated;
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
    }

    private void UpdateTimer()
    {
        if (enabled && !paused && !timer.IsRunning) timer.Start();
        if ((!enabled || paused) && timer.IsRunning) timer.Stop();
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
            jobs.Clear();
            lastLibraryJob = null;
            UpdateRequiredPackages();
            SetStatus("У бібліотеці ще немає документів.");
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
        try
        {
            var current = session.Document?.Id;
            var preferences = session.GetReaderPreferences();
            var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
            var live = new HashSet<JobKey>();
            var ready = new List<(JobKey Key, Document Document)>();
            string? missing = null;
            var waiting = false;
            foreach (var document in session.Snapshot.Documents.OrderByDescending(doc => doc.Id == current))
            {
                if (document.CurrentRevisionId is not { } revision) continue;
                var target = preferences.TranslationTargets.GetValueOrDefault(document.Language)
                    ?? (document.Language == "uk" ? "fr" : "uk");
                if (target == document.Language) continue;
                var key = new JobKey(document.Id, revision, target);
                live.Add(key);
                if (!jobs.TryGetValue(key, out var job) || job.ResumeAt is { } deadline && deadline <= now)
                    ready.Add((key, document));
                else if (job.MissingPackages is { } packages)
                    missing ??= packages.Length == 0
                        ? $"Офлайн-маршрут недоступний · {PairName(document.Language, target)}."
                        : $"Потрібні мовні пакети · {string.Join(", ", packages.Select(pair => PairName(pair.Source, pair.Target)))}.";
                else if (job.ResumeAt is not null)
                    waiting = true;
            }
            foreach (var key in jobs.Keys.Where(key => !live.Contains(key)).ToArray()) jobs.Remove(key);
            UpdateRequiredPackages();
            if (visiblePdf is { } visible && current == visible.Address.DocumentId &&
                (visibleAttempt is not { } attempted || attempted.Page != visible || attempted.RetryAt <= now))
            {
                var address = visible.Address;
                var key = new JobKey(address.DocumentId, address.RevisionId, address.Target);
                if (live.Contains(key) && jobs.GetValueOrDefault(key)?.MissingPackages is null)
                {
                    SetStatus($"Опрацьовуємо відкриту сторінку {address.Page + 1}…", running: true);
                    visibleAttempt = (visible, now + 30);
                    var complete = await session.PrefetchVisiblePdfTranslationAsync(address);
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
                SetStatus(missing ?? (waiting ? "Очікує на доступний текст PDF або повторну спробу."
                    : "Чергу перекладу опрацьовано."));
                return;
            }

            var previous = ready.FindIndex(candidate => candidate.Key == lastLibraryJob);
            var work = ready[(previous + 1) % ready.Count];
            var title = work.Document.Title;
            lastLibraryJob = work.Key;
            libraryStepInvalidated = false;
            SetStatus($"Опрацьовуємо «{title}»…", running: true);
            var progress = await session.AdvanceBackgroundTranslationAsync(work.Key.Document, work.Key.Revision, work.Key.Target);
            if (!enabled || paused || libraryStepInvalidated) return;
            jobs[work.Key] = progress;
            UpdateRequiredPackages();
#if MACCATALYST
            if (progress.PendingPdfTextPage is int pendingPage &&
                work.Document.PdfPageStarts?.Length is int pageCount &&
                await session.OriginalFileAsync(work.Key.Document) is { } original)
            {
                if (!enabled || paused || libraryStepInvalidated) return;
                SetStatus($"Читаємо текстовий шар сторінки {pendingPage + 1} · «{title}»…", running: true);
                var text = await PdfScan.ReadPageTextAsync(original.Path, pendingPage, pageCount, CancellationToken.None);
                if (!enabled || paused || libraryStepInvalidated) return;
                await session.CacheBackgroundPdfTextLayerAsync(work.Key.Document, work.Key.Revision, pendingPage, text);
                if (enabled && !paused) SetStatus("Черга перекладу продовжується.");
                return;
            }
#endif
            SetStatus(progress.MissingPackages is not null
                ? $"Для «{title}» бракує офлайн-маршруту."
                : progress.TranslatedPage is int page
                    ? $"Переклад сторінки {page + 1} · «{title}»."
                    : $"Перевірено кеш · «{title}».");
        }
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error);
            retryAt = DateTimeOffset.UtcNow.AddSeconds(10);
            if (enabled && !paused) SetStatus("Фоновий переклад повторить спробу.");
        }
    }
}
