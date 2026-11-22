using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record DetachedAddress(string Document, int Page, string Text, string Language);
    private readonly LocalSpeech detachedSpeech = new();
    private DetachedAddress? detachedAddress;
    private DetachedPdfSentence? detachedSentence;
    private bool loadingDetachedSentence;
    private bool translatingDetachedSentence;
    private bool speakingDetachedSentence;
    private bool detachedPaused;
    private int detachedAudioRun;
    private CancellationTokenSource? detachedAudioCancellation;
    private int detachedTranslationRun;

    private void ShowDetachedPdfPage(Document document, PdfReadPage page)
    {
        TargetLanguage.IsEnabled = !AppState.Session.Busy;
        var address = new DetachedAddress(document.Id, page.Page, page.Text, document.Language);
        if (detachedAddress == address) return;

        StopDetachedPdfReader();
        detachedAddress = address;
        _ = LoadDetachedSentenceAsync(address, position: null);
    }

    private void StopDetachedPdfReader()
    {
        if (detachedAddress is null) return;
        detachedAddress = null;
        detachedSentence = null;
        loadingDetachedSentence = false;
        ClearDetachedSpeech();
        ResetDetachedTranslation();
    }

    private async Task LoadDetachedSentenceAsync(DetachedAddress address, int? position)
    {
        loadingDetachedSentence = true;
        ClearDetachedSpeech();
        ResetDetachedTranslation();
        try
        {
            var result = await AppState.Session.DetachedPdfSentenceAsync(address.Document, address.Page, position);
            if (!ReferenceEquals(detachedAddress, address)) return;
            detachedSentence = result;
            if (result is not { } current) return;
            var sentence = current.Sentence;
            DetachedPdfText.HighlightStart = sentence.StartUtf16;
            DetachedPdfText.HighlightLength = sentence.EndUtf16 - sentence.StartUtf16;
            DetachedPdfText.RevealOffset(sentence.StartUtf16);
            DetachedSentenceStatus.Text = $"Речення {current.Position + 1} із {current.Total}";
        }
        catch (Exception error)
        {
            if (ReferenceEquals(detachedAddress, address)) DetachedSentenceStatus.Text = error.Message;
        }
        finally
        {
            if (ReferenceEquals(detachedAddress, address))
            {
                loadingDetachedSentence = false;
                UpdateDetachedControls();
            }
        }
    }

    private async void PreviousDetachedSentenceClicked(object? sender, EventArgs args) => await MoveDetachedSentenceAsync(-1);
    private async void NextDetachedSentenceClicked(object? sender, EventArgs args) => await MoveDetachedSentenceAsync(1);

    private Task MoveDetachedSentenceAsync(int change)
    {
        if (loadingDetachedSentence || detachedAddress is not { } address || detachedSentence is not { } current)
            return Task.CompletedTask;
        var next = current.Position + change;
        return next >= 0 && next < current.Total ? LoadDetachedSentenceAsync(address, next) : Task.CompletedTask;
    }

    private async void DetachedTranslateClicked(object? sender, EventArgs args)
    {
        if (!DetachedTranslate.IsEnabled || detachedAddress is not { } address ||
            detachedSentence is not { } current || TargetCode is not { } target) return;
        var run = ++detachedTranslationRun;
        translatingDetachedSentence = true;
        UpdateDetachedControls();
        ShowDetachedTranslation("Перекладаємо офлайн…");
        try
        {
            var translated = await AppState.Session.TranslateDetachedPdfSentenceAsync(
                address.Document, address.Page, current.Sentence.Id, target);
            if (run != detachedTranslationRun) return;
            var text = translated?.Text ?? "Для цього напрямку потрібен пакет офлайн-перекладу.";
            if (translated?.PivotLanguage is { } pivot)
                text = $"{DocumentLanguages.PivotRoute(current.Language, pivot, target)}\n{text}";
            ShowDetachedTranslation(text);
        }
        catch (Exception error)
        {
            if (run == detachedTranslationRun) ShowDetachedTranslation(error.Message);
        }
        finally
        {
            if (run == detachedTranslationRun)
            {
                translatingDetachedSentence = false;
                UpdateDetachedControls();
            }
        }
    }

    private void ResetDetachedTranslation()
    {
        detachedTranslationRun++;
        translatingDetachedSentence = false;
        ShowDetachedTranslation("");
        UpdateDetachedControls();
    }

    private void ShowDetachedTranslation(string value)
    {
        DetachedTranslation.Text = value;
        DetachedTranslationPane.IsVisible = value.Length != 0;
    }

    private async void DetachedSpeakClicked(object? sender, EventArgs args)
    {
        if (loadingDetachedSentence || detachedAddress is not { } address || detachedSentence is not { } current) return;
        if (speakingDetachedSentence)
        {
            if (detachedPaused ? detachedSpeech.Resume() : detachedSpeech.Pause())
            {
                detachedPaused = !detachedPaused;
                UpdateDetachedControls();
            }
            else if (!detachedPaused) ClearDetachedSpeech();
            return;
        }

        using var cancellation = new CancellationTokenSource();
        detachedAudioCancellation = cancellation;
        var token = cancellation.Token;
        var run = ++detachedAudioRun;
        speakingDetachedSentence = true;
        var rate = SpeechRatePercent / 100f;
        detachedSpeech.Rate = rate;
        UpdateDetachedControls();
        var sentence = current.Sentence;
        try
        {
            await loadingVoices.WaitAsync(token);
            if (run != detachedAudioRun) return;
            var voice = SelectedVoice();
            if (voice?.Engine is { } engine)
            {
                var session = AppState.Session;
                var source = new SpeechSource(address.Document, sentence.Id, current.Language, sentence.Text, sentence.Text);
                var key = new ClipKey(source, engine, voice.Speaker, rate);
                DetachedSentenceStatus.Text = "Готуємо локальне озвучення…";
                var audio = await PrepareAudio(session, key, token, address.Page).WaitAsync(token);
                if (run != detachedAudioRun) return;
                DetachedSentenceStatus.Text = $"Речення {current.Position + 1} із {current.Total}";
                await detachedSpeech.PlayWaveAsync(audio, 0, startedAt =>
                {
                    if (!token.IsCancellationRequested && ContinueSpeech.IsChecked)
                        _ = PrefetchNextAudioAsync(session, key, token, address.Page);
                });
                return;
            }

            detachedSpeech.PreferredVoice = voice?.Id == SystemDefaultVoice ? null : voice?.Id;
            await detachedSpeech.SpeakAsync(sentence.Text, current.Language, (start, length) =>
                MainThread.BeginInvokeOnMainThread(() =>
                {
                    if (run != detachedAudioRun || start < 0 || length <= 0 || start > sentence.Text.Length - length) return;
                    DetachedPdfText.SpokenWord = new TextSelection(
                        sentence.StartUtf16 + start, length, sentence.Text.Substring(start, length));
                }));
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) {}
        catch (Exception error)
        {
            if (run == detachedAudioRun) DetachedSentenceStatus.Text = error.Message;
        }
        finally
        {
            if (run == detachedAudioRun) ClearDetachedSpeech();
        }
    }

    private void ClearDetachedSpeech()
    {
        detachedAudioRun++;
        var running = detachedAudioCancellation;
        detachedAudioCancellation = null;
        running?.Cancel();
        detachedSpeech.Stop();
        speakingDetachedSentence = detachedPaused = false;
        DetachedPdfText.SpokenWord = null;
        UpdateDetachedControls();
    }

    private void UpdateDetachedControls()
    {
        var available = !loadingDetachedSentence && detachedSentence is not null;
        DetachedPrevious.IsEnabled = available && detachedSentence is { Position: > 0 };
        DetachedNext.IsEnabled = available && detachedSentence is { } current && current.Position + 1 < current.Total;
        DetachedSpeak.IsEnabled = available;
        DetachedTranslate.IsEnabled = available && !translatingDetachedSentence && TargetCode is not null;
        var playing = speakingDetachedSentence && !detachedPaused;
        DetachedSpeak.Text = playing ? "Ⅱ" : "▶";
        SemanticProperties.SetDescription(DetachedSpeak, playing ? "Призупинити озвучення" : "Озвучити речення сторінки");
    }
}
