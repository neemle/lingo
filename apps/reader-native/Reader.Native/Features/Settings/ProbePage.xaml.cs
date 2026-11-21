using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class ProbePage : ContentPage
{
    private readonly LocalSpeech speech = new();
    private const string Sample = "Don't stop. L’esprit. Cafe\u0301 👩🏽‍💻 😀.\r\n中文 日本語。\nمرحبا بالعالم. שלום עולם.";
    public ProbePage()
    {
        InitializeComponent();
        ProbeText.Text = Sample;
        ProbeText.HighlightStart = Sample.IndexOf("Cafe", StringComparison.Ordinal);
        ProbeText.HighlightLength = "Cafe\u0301".Length;
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
        speech.Rate = (float)Rate.Value;
        SpeechResult.Text = "Озвучення речення: Don't stop.";
        await speech.SpeakAsync("Don't stop.", "en");
        SpeechResult.Text = speech.Status;
    }
    private void StopClicked(object? sender, EventArgs args) { speech.Stop(); SpeechResult.Text = "Зупинено."; }
    protected override void OnDisappearing() { speech.Stop(); base.OnDisappearing(); }
}
