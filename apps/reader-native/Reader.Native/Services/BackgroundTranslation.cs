using Reader.Presentation;

namespace Reader.Native.Services;

internal sealed class BackgroundTranslation
{
    private readonly ReaderSession session;
    private readonly IDispatcherTimer timer;
    private readonly Dictionary<JobKey, long> deadlines = [];
    private Task pending = Task.CompletedTask;
    private DateTimeOffset retryAt;
    private bool enabled;
    private bool paused;

    private sealed record JobKey(string Document, string Revision, string Target);

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
        if (!value) deadlines.Clear();
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
            deadlines.Clear();
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
            var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
            var live = new HashSet<JobKey>();
            JobKey? candidate = null;
            foreach (var document in session.Snapshot.Documents.OrderByDescending(doc => doc.Id == current))
            {
                if (document.CurrentRevisionId is not { } revision) continue;
                var target = preferences.TranslationTargets.GetValueOrDefault(document.Language)
                    ?? (document.Language == "uk" ? "fr" : "uk");
                if (target == document.Language) continue;
                var key = new JobKey(document.Id, revision, target);
                live.Add(key);
                if (candidate is null && deadlines.GetValueOrDefault(key) <= now)
                    candidate = key;
            }
            foreach (var key in deadlines.Keys.Where(key => !live.Contains(key)).ToArray()) deadlines.Remove(key);
            if (candidate is not { } work) return;

            var progress = await session.AdvanceBackgroundTranslationAsync(work.Document, work.Revision, work.Target);
            if (enabled && !paused) deadlines[work] = progress.ResumeAt ?? long.MaxValue;
        }
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine(error);
            retryAt = DateTimeOffset.UtcNow.AddSeconds(10);
        }
    }
}
