using System.ComponentModel;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage : ContentPage
{
    private readonly LocalSpeech speech = new();
    private TextSelection? selection;
    private string? documentId;
    public ReadingPage() => InitializeComponent();

    protected override void OnAppearing()
    {
        base.OnAppearing();
        BindingContext = AppState.Session;
        AppState.Session.PropertyChanged += SessionChanged;
        UpdateHighlight();
    }

    protected override void OnDisappearing()
    {
        speech.Stop();
        AppState.Session.PropertyChanged -= SessionChanged;
        base.OnDisappearing();
    }

    private void SessionChanged(object? sender, PropertyChangedEventArgs args)
    {
        UpdateHighlight();
        if (args.PropertyName == nameof(ReaderSession.Error) && AppState.Session.Error.Length > 0)
            SemanticScreenReader.Default.Announce(AppState.Session.Error);
    }
    private void UpdateHighlight()
    {
        var sentence = AppState.Session.Sentence;
        SourceText.HighlightLength = sentence is null ? 0 : sentence.EndUtf16 - sentence.StartUtf16;
        if (documentId == AppState.Session.Document?.Id) return;
        documentId = AppState.Session.Document?.Id;
        selection = null;
        WordCard.IsVisible = false;
        Translation.Text = "";
    }

    private async void SourceSelected(object? sender, TextSelection selected)
    {
        if (selection != selected) Translation.Text = "";
        selection = selected;
        SelectedWord.Text = selected.Text;
        RangeLabel.Text = $"Джерело UTF-16: {selected.StartUtf16} + {selected.LengthUtf16}";
        WordCard.IsVisible = true;
        if (AppState.Session.Document is not { } doc) return;
        var index = Array.FindIndex(doc.Sentences, sentence => selected.StartUtf16 >= sentence.StartUtf16
            && selected.StartUtf16 < sentence.EndUtf16);
        if (index >= 0 && index != doc.Position) { speech.Stop(); await AppState.Session.SelectSentenceAsync(index); }
    }

    private async void SaveWordClicked(object? sender, EventArgs args)
    {
        if (selection is null) return;
        await AppState.Session.SaveWordAsync(selection, Translation.Text ?? "");
        if (AppState.Session.Error.Length == 0) { WordCard.IsVisible = false; Translation.Text = ""; }
    }

    private async void PreviousClicked(object? sender, EventArgs args)
    {
        speech.Stop(); await AppState.Session.MoveAsync(-1);
    }
    private async void NextClicked(object? sender, EventArgs args)
    {
        speech.Stop(); await AppState.Session.MoveAsync(1);
    }
    private async void SentenceChanged(object? sender, EventArgs args)
    {
        if (SentencePicker.SelectedIndex < 0 || AppState.Session.Busy) return;
        speech.Stop(); await AppState.Session.SelectSentenceAsync(SentencePicker.SelectedIndex);
    }

    private async void SpeakClicked(object? sender, EventArgs args)
    {
        if (AppState.Session.Sentence is not { } sentence || AppState.Session.Document is not { } doc) return;
        speech.Rate = Rate.SelectedIndex switch { 0 => .75f, 2 => 1.25f, _ => 1f };
        SpeechStatus.Text = "Озвучення поточного речення…";
        await speech.SpeakAsync(sentence.Text, doc.Language);
        SpeechStatus.Text = speech.Status;
    }
    private void StopClicked(object? sender, EventArgs args)
    {
        speech.Stop(); SpeechStatus.Text = "Озвучення зупинено.";
    }
}
