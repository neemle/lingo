using Reader.Presentation;

namespace Reader.Native.Services;

internal sealed class BackgroundTranslation
{
    private readonly ReaderSession session;
    private readonly IDispatcherTimer timer;
    private readonly Dictionary<JobKey, Job> jobs = [];
    private Task pending = Task.CompletedTask;
    private DateTimeOffset retryAt;
    private bool enabled;
    private bool paused;

    private sealed record JobKey(string Document, string Revision, string Target);

    private sealed class Job
    {
        public int Cursor { get; set; }
        public bool Incomplete { get; set; }
        public DateTimeOffset ResumeAt { get; set; }
    }

    internal BackgroundTranslation(ReaderSession session, IDispatcher dispatcher)
    {
        this.session = session;
        timer = dispatcher.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(2);
        timer.Tick += Tick;
        SetEnabled(session.GetReaderPreferences().BackgroundTranslation == true);
    }

    internal void SetEnabled(bool value)
    {
        enabled = value;
        if (!value) jobs.Clear();
        UpdateTimer();
    }

    internal async Task PauseAsync()
    {
        paused = true;
        UpdateTimer();
        await pending;
    }

    internal void Resume(bool clearProgress = false)
    {
        if (clearProgress)
        {
            jobs.Clear();
            retryAt = default;
        }
        paused = false;
        UpdateTimer();
    }

    internal void Stop()
    {
        SetEnabled(false);
        timer.Tick -= Tick;
    }

    private void UpdateTimer()
    {
        if (enabled && !paused && !timer.IsRunning) timer.Start();
        if ((!enabled || paused) && timer.IsRunning) timer.Stop();
    }

    private void Tick(object? sender, EventArgs args)
    {
        if (enabled && !paused && !session.Busy && pending.IsCompleted &&
            retryAt <= DateTimeOffset.UtcNow && session.Snapshot.Documents.Length > 0)
            pending = RunStepAsync();
    }

    private async Task RunStepAsync()
    {
        try
        {
            var current = session.Document?.Id;
            var preferences = session.GetReaderPreferences();
            var now = DateTimeOffset.UtcNow;
            var live = new HashSet<JobKey>();
            (JobKey Key, Job Job, string Language, bool IsPdf)? candidate = null;
            foreach (var document in session.Snapshot.Documents.OrderByDescending(doc => doc.Id == current))
            {
                if (document.CurrentRevisionId is not { } revision) continue;
                var target = preferences.TranslationTargets.GetValueOrDefault(document.Language)
                    ?? (document.Language == "uk" ? "fr" : "uk");
                if (target == document.Language) continue;
                var key = new JobKey(document.Id, revision, target);
                live.Add(key);
                if (!jobs.TryGetValue(key, out var job)) jobs.Add(key, job = new Job());
                if (candidate is null && job.ResumeAt <= now)
                    candidate = (key, job, document.Language, document.Format == "pdf");
            }
            foreach (var key in jobs.Keys.Where(key => !live.Contains(key)).ToArray()) jobs.Remove(key);
            if (candidate is not { } work) return;

            if (work.Job.Cursor == 0 && !(await session.TranslationReadinessAsync(work.Language, work.Key.Target)).Ready)
            {
                work.Job.ResumeAt = now.AddSeconds(30);
                return;
            }
            if (!enabled || paused) return;
            var step = await session.WarmTranslationStepAsync(
                work.Key.Document, work.Key.Revision, work.Key.Target, work.Job.Cursor);
            if (step.Next is { } next)
            {
                work.Job.Cursor = next;
                work.Job.Incomplete |= step.Incomplete;
                return;
            }
            work.Job.ResumeAt = work.Job.Incomplete || work.IsPdf ? now.AddMinutes(5) : DateTimeOffset.MaxValue;
            work.Job.Cursor = 0;
            work.Job.Incomplete = false;
        }
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error);
            retryAt = DateTimeOffset.UtcNow.AddSeconds(10);
        }
    }
}
