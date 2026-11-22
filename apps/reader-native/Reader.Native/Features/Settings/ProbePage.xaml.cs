using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class ProbePage : ContentPage
{
    private readonly LocalSpeech speech = new();
    private SpeechVoice[] voices = [];
    private int speechRequest;
    private const string SpokenSample = "Don't stop at Cafe\u0301 now.";
    private const string Sample = SpokenSample + " L’esprit. 👩🏽‍💻 😀.\r\n中文 日本語。\nمرحبا بالعالم. שלום עולם.";
    public ProbePage()
    {
        InitializeComponent();
        ProbeText.Text = Sample;
        ProbeText.HighlightStart = Sample.IndexOf("Cafe", StringComparison.Ordinal);
        ProbeText.HighlightLength = "Cafe\u0301".Length;
    }

    protected override void OnAppearing()
    {
        base.OnAppearing();
        try
        {
            var selected = voices.ElementAtOrDefault(VoicePicker.SelectedIndex)?.Id;
            voices = LocalSpeech.VoicesForLanguage("en");
            VoicePicker.ItemsSource = voices.Select(voice => $"{voice.Name} · {voice.Language}").ToArray();
            VoicePicker.SelectedIndex = voices.Length == 0 ? -1 : Math.Max(0, Array.FindIndex(voices, voice => voice.Id == selected));
            PlaySpeech.IsEnabled = voices.Length > 0;
            if (!PlaySpeech.IsEnabled) SpeechResult.Text = "Системний голос для англійської не встановлено.";
        }
        catch (Exception error)
        {
            PlaySpeech.IsEnabled = false;
            SpeechResult.Text = error.Message;
        }
    }

    private void Selected(object? sender, TextSelection selection)
    {
        var exact = Sample.Substring(selection.StartUtf16, selection.LengthUtf16) == selection.Text;
        Result.Text = $"{DateTimeOffset.Now:O} · {DeviceInfo.Platform} · {DeviceInfo.VersionString}\n"
            + $"UTF-16 start={selection.StartUtf16}, length={selection.LengthUtf16}, sourceMatch={exact}\n"
            + $"«{selection.Text}»\n" + Result.Text;
    }

    private async void SpeakClicked(object? sender, EventArgs args)
    {
        if (voices.ElementAtOrDefault(VoicePicker.SelectedIndex) is not { } voice) return;
        speech.Rate = (float)Rate.Value;
        speech.PreferredVoice = voice.Id;
        var request = ++speechRequest;
        ProbeText.SpokenWord = null;
        ProbeText.HighlightStart = 0;
        ProbeText.HighlightLength = SpokenSample.Length;
        var boundaries = new List<string>();
        SpeechResult.Text = $"Озвучує {voice.Name}…";
        var status = await speech.SpeakAsync(SpokenSample, "en", (start, length) => Dispatcher.Dispatch(() =>
        {
            if (request != speechRequest || SourceRanges.Selection(SpokenSample, start, length) is not { } word) return;
            ProbeText.SpokenWord = word;
            boundaries.Add($"«{word.Text}» [{word.StartUtf16}+{word.LengthUtf16}]");
            SpeechResult.Text = string.Join(" · ", boundaries);
        }));
        if (request != speechRequest || status is null) return;
        speechRequest++;
        ProbeText.SpokenWord = null;
        var report = boundaries.Count == 0 ? "Межі слів від голосу не отримано." : string.Join(" · ", boundaries);
        SpeechResult.Text = $"{voice.Name} · {status}\n{report}";
    }
    private void StopClicked(object? sender, EventArgs args)
    {
        speechRequest++;
        speech.Stop();
        ProbeText.SpokenWord = null;
        SpeechResult.Text = "Зупинено.";
    }
    protected override void OnDisappearing()
    {
        speechRequest++;
        speech.Stop();
        ProbeText.SpokenWord = null;
        base.OnDisappearing();
    }
}
