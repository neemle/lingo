using Reader.Presentation;
using Windows.Foundation.Collections;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Media.SpeechSynthesis;

namespace Reader.Native.Services;

internal sealed class NativeSpeech
{
    private MediaPlayer? active;

    internal bool Pause()
    {
        if (active is null) return false;
        active.Pause();
        return true;
    }

    internal bool Resume()
    {
        if (active is null) return false;
        active.Play();
        return true;
    }

    internal static SpeechVoice[] InstalledVoices() => SpeechSynthesizer.AllVoices.Select(ToVoice).ToArray();

    private static SpeechVoice ToVoice(VoiceInformation voice) =>
        new(voice.Id, voice.DisplayName, voice.Language);

    internal async Task SpeakAsync(string text, string language, string? preferredVoice, float rate,
        Action<int, int>? boundary, CancellationToken token)
    {
        var voices = SpeechSynthesizer.AllVoices;
        var selected = SpeechVoice.ResolveId(voices.Select(ToVoice), language, preferredVoice,
            SpeechSynthesizer.DefaultVoice?.Id);
        var voice = voices.FirstOrDefault(item => item.Id == selected)
            ?? throw new InvalidOperationException("Голос цієї мови не встановлено в системі.");
        using var engine = new SpeechSynthesizer { Voice = voice };
        engine.Options.SpeakingRate = rate;
        engine.Options.IncludeWordBoundaryMetadata = boundary is not null;
        using var stream = await engine.SynthesizeTextToStreamAsync(text).AsTask(token);
        token.ThrowIfCancellationRequested();
        using var source = MediaSource.CreateFromStream(stream, stream.ContentType);
        var item = new MediaPlaybackItem(source);
        using var player = new MediaPlayer();
        var completed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var tracks = new HashSet<TimedMetadataTrack>();
        var trackLock = new object();
        var closed = false;

        void CueEntered(TimedMetadataTrack track, MediaCueEventArgs args)
        {
            if (token.IsCancellationRequested || args.Cue is not SpeechCue cue) return;
            var start = cue.StartPositionInInput;
            var end = cue.EndPositionInInput;
            if (start <= end && end < text.Length)
                boundary?.Invoke((int)start, (int)(end - start + 1));
        }

        void RegisterTracks(MediaPlaybackItem sender, IVectorChangedEventArgs? _)
        {
            lock (trackLock)
            {
                if (closed) return;
                var current = sender.TimedMetadataTracks;
                for (int index = 0; index < current.Count; index++)
                {
                    var track = current[index];
                    if (track.TimedMetadataKind != TimedMetadataKind.Speech ||
                        track.Label != "SpeechWord" || !tracks.Add(track)) continue;
                    track.CueEntered += CueEntered;
                    current.SetPresentationMode((uint)index, TimedMetadataTrackPresentationMode.ApplicationPresented);
                }
            }
        }

        if (boundary is not null)
        {
            item.TimedMetadataTracksChanged += RegisterTracks;
            RegisterTracks(item, null);
        }
        player.MediaEnded += (_, _) => completed.TrySetResult();
        player.MediaFailed += (_, args) => completed.TrySetException(new InvalidOperationException(args.ErrorMessage));
        using var registration = token.Register(() => completed.TrySetCanceled(token));
        active = player;
        try
        {
            token.ThrowIfCancellationRequested();
            player.Source = item;
            player.Play();
            await completed.Task;
        }
        finally
        {
            active = null;
            item.TimedMetadataTracksChanged -= RegisterTracks;
            lock (trackLock)
            {
                closed = true;
                foreach (var track in tracks) track.CueEntered -= CueEntered;
            }
            player.Source = null;
        }
    }
}
