using System.Diagnostics;
using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private static readonly TimeSpan ReadingIdleLimit = TimeSpan.FromMinutes(1);
    private Window? readingHistoryWindow;
    private bool readingHistoryVisible;
    private bool readingHistoryForeground;
    private ReadingVisit? readingVisit;

    private sealed class ReadingVisit(ReadingSession initial, int position)
    {
        public ReadingSession Checkpoint { get; set; } = initial;
        public int Position { get; set; } = position;
        public long? LastInteraction { get; set; } = Stopwatch.GetTimestamp();
        public TimeSpan Active { get; set; }
        public bool RetryCheckpoint { get; set; }
    }

    private void BeginReadingHistory()
    {
        if (readingHistoryVisible) return;
        readingHistoryVisible = true;
        readingHistoryForeground = true;
        readingHistoryWindow = Window;
        if (readingHistoryWindow is not null)
        {
            readingHistoryWindow.Activated += ReadingHistoryActivated;
            readingHistoryWindow.Deactivated += ReadingHistoryPaused;
            readingHistoryWindow.Stopped += ReadingHistoryPaused;
        }
        SyncReadingHistory();
    }

    private void EndReadingHistory(bool keepVisit)
    {
        readingHistoryVisible = false;
        readingHistoryForeground = false;
        if (readingHistoryWindow is not null)
        {
            readingHistoryWindow.Activated -= ReadingHistoryActivated;
            readingHistoryWindow.Deactivated -= ReadingHistoryPaused;
            readingHistoryWindow.Stopped -= ReadingHistoryPaused;
            readingHistoryWindow = null;
        }
        ResetReadingActivity();
        if (!keepVisit) FinishReadingVisit();
    }

    private void ReadingHistoryActivated(object? sender, EventArgs args)
    {
        readingHistoryForeground = true;
        SyncReadingHistory();
    }

    private void ReadingHistoryPaused(object? sender, EventArgs args)
    {
        readingHistoryForeground = false;
        ResetReadingActivity();
        CheckpointSpeechPosition();
    }

    private void ResetReadingActivity()
    {
        if (readingVisit is { } visit) visit.LastInteraction = null;
    }

    private void SyncReadingHistory()
    {
        if (!readingHistoryVisible || !readingHistoryForeground) return;
        if (AppState.Session.Document is not { } document)
        {
            FinishReadingVisit();
            return;
        }
        if (readingVisit is { } visit && visit.Checkpoint.DocumentId == document.Id)
        {
            if (visit.Position != document.Position)
            {
                visit.Position = document.Position;
                RecordReadingActivity();
            }
            return;
        }
        FinishReadingVisit();
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var opened = new ReadingSession(Guid.NewGuid().ToString("D"), document.Id, now, now, 0);
        readingVisit = new ReadingVisit(opened, document.Position);
        _ = SaveReadingCheckpointAsync(readingVisit, opened);
    }

    private void RecordReadingActivity()
    {
        if (!readingHistoryVisible || !readingHistoryForeground || readingVisit is not { } visit) return;
        var timestamp = Stopwatch.GetTimestamp();
        if (visit.LastInteraction is long previous)
        {
            var elapsed = Stopwatch.GetElapsedTime(previous, timestamp);
            if (elapsed <= ReadingIdleLimit)
                visit.Active += elapsed;
        }
        visit.LastInteraction = timestamp;
        CheckpointReadingVisit(visit);
    }

    private void FinishReadingVisit()
    {
        if (readingVisit is not { } visit) return;
        readingVisit = null;
        CheckpointReadingVisit(visit);
    }

    private void CheckpointReadingVisit(ReadingVisit visit)
    {
        var checkpoint = visit.Checkpoint;
        var end = Math.Max(checkpoint.EndedAt, DateTimeOffset.UtcNow.ToUnixTimeSeconds());
        var activeSeconds = Math.Min((long)visit.Active.TotalSeconds, end - checkpoint.StartedAt);
        var current = checkpoint with { EndedAt = end, ActiveDurationSeconds = activeSeconds };
        if (current == checkpoint && !visit.RetryCheckpoint) return;
        visit.Checkpoint = current;
        visit.RetryCheckpoint = false;
        _ = SaveReadingCheckpointAsync(visit, current);
    }

    private static async Task SaveReadingCheckpointAsync(ReadingVisit visit, ReadingSession checkpoint)
    {
        try { await AppState.Session.CheckpointReadingAsync(checkpoint); }
        catch (Exception error)
        {
            visit.RetryCheckpoint = true;
            Debug.WriteLine($"Reading history: {error.Message}");
        }
    }
}
