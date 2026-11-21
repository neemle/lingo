using System.ComponentModel;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage : ContentPage
{
    private static readonly (string Code, string Name)[] TranslationLanguages =
    [
        ("uk", "Українська"), ("en", "Англійська"), ("fr", "Французька"), ("de", "Німецька"),
        ("es", "Іспанська"), ("it", "Італійська"), ("pt", "Португальська"), ("pl", "Польська"),
        ("nl", "Нідерландська"), ("cs", "Чеська"), ("ro", "Румунська"), ("sv", "Шведська"),
    ];
    private (string Code, string Name)[] availableTargets = [];
    private string? lastSourceLanguage;
    private bool updatingTargets;
    private bool stacked;
    private string? TargetCode
    {
        get
        {
            var index = TargetLanguage.SelectedIndex;
            return index >= 0 && index < availableTargets.Length ? availableTargets[index].Code : null;
        }
    }
    private readonly LocalSpeech speech = new();
    private TextSelection? selection;
    private string? documentId;
    private int speechRequest;
    public ReadingPage() => InitializeComponent();

    protected override void OnAppearing()
    {
        base.OnAppearing();
        BindingContext = AppState.Session;
        AppState.Session.PropertyChanged += SessionChanged;
        UpdateHighlight();
        UpdateTranslation();
    }

    protected override void OnDisappearing()
    {
        StopSpeech();
        AppState.Session.PropertyChanged -= SessionChanged;
        base.OnDisappearing();
    }

    private void SessionChanged(object? sender, PropertyChangedEventArgs args)
    {
        // Notifications describe a completed session state. Update once per state change.
        if (args.PropertyName == nameof(ReaderSession.Snapshot))
        {
            UpdateHighlight();
            UpdateTranslation();
        }
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

    protected override void OnSizeAllocated(double width, double height)
    {
        base.OnSizeAllocated(width, height);
        if (ReadingColumns is null || TranslationPane is null || width <= 0 || stacked == (width < 800)) return;

        stacked = width < 800;
        ReadingColumns.ColumnDefinitions[1].Width = stacked ? 0 : new GridLength(2, GridUnitType.Star);
        ReadingColumns.RowDefinitions[1].Height = stacked ? new GridLength(1, GridUnitType.Star) : 0;
        ReadingColumns.RowSpacing = stacked ? 12 : 0;
        Grid.SetColumn(TranslationPane, stacked ? 0 : 1);
        Grid.SetRow(TranslationPane, stacked ? 1 : 0);
    }

    private void UpdateTranslation()
    {
        var session = AppState.Session;
        var source = session.Document?.Language;
        if (source != lastSourceLanguage)
        {
            var previous = TargetCode;
            lastSourceLanguage = source;
            updatingTargets = true;
            try
            {
                availableTargets = TranslationLanguages.Where(language => language.Code != source).ToArray();
                TargetLanguage.ItemsSource = availableTargets.Select(language => language.Name).ToArray();
                var previousIndex = Array.FindIndex(availableTargets, language => language.Code == previous);
                TargetLanguage.SelectedIndex = Math.Max(0, previousIndex);
            }
            finally { updatingTargets = false; }
        }

        var active = session.Sentence is not null && TargetCode is not null;
        var text = TargetCode is { } target ? session.ReviewedTranslation(target) : null;
        TranslationNotice.Text = !active ? "Відкрийте документ для читання."
            : text is null ? "Перекладу ще немає. Можна додати свій."
            : "Ваш переклад активного речення";
        SegmentTranslation.Text = text ?? "";
        EditTranslation.Text = text is null ? "Додати свій переклад" : "Змінити переклад";
        EditTranslation.IsEnabled = active && !session.Busy;
        TargetLanguage.IsEnabled = active && !session.Busy;
    }

    private void TargetChanged(object? sender, EventArgs args)
    {
        if (TranslationNotice is not null && !updatingTargets) UpdateTranslation();
    }

    private async void EditTranslationClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (session.Document is not { } document || session.Sentence is not { } sentence
            || TargetCode is not { } target || session.Busy)
            return;

        var existing = session.ReviewedTranslation(target);
        var text = await DisplayPromptAsync("Переклад речення",
            existing is null ? sentence.Text : "Очистьте поле, щоб видалити цей переклад.",
            accept: "Зберегти", cancel: "Скасувати", maxLength: 4000, initialValue: existing ?? "");
        if (text is not null && session.Document?.Id == document.Id && session.Sentence?.Id == sentence.Id
            && TargetCode == target && !session.Busy)
            await session.SetReviewedTranslationAsync(document.Id, sentence.Id, target, text);
    }

    private async void SourceSelected(object? sender, TextSelection selected)
    {
        if (selection != selected) Translation.Text = "";
        selection = selected;
        SelectedWord.Text = selected.Text;
        WordCard.IsVisible = true;
        if (AppState.Session.Document is not { } doc) return;
        var index = Array.FindIndex(doc.Sentences, sentence => selected.StartUtf16 >= sentence.StartUtf16
            && selected.StartUtf16 < sentence.EndUtf16);
        if (index >= 0 && index != doc.Position) { StopSpeech(); await AppState.Session.SelectSentenceAsync(index); }
    }

    private async void SaveWordClicked(object? sender, EventArgs args)
    {
        if (selection is null) return;
        await AppState.Session.SaveWordAsync(selection, Translation.Text ?? "");
        if (AppState.Session.Error.Length == 0) { WordCard.IsVisible = false; Translation.Text = ""; }
    }

    private async void BookmarkClicked(object? sender, EventArgs args) =>
        await AppState.Session.ToggleBookmarkAsync();

    private async void OpenBookmarksClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (session.Document is not { } document || !session.HasBookmarks) return;
        var saved = document.Bookmarks.ToHashSet(StringComparer.Ordinal);
        var choices = document.Sentences.Select((sentence, index) => (sentence, index))
            .Where(item => saved.Contains(item.sentence.Id))
            .ToDictionary(item => BookmarkLabel(item.index, item.sentence.Text), item => item.index);

        var selected = await DisplayActionSheetAsync("Закладки", "Скасувати", null, choices.Keys.ToArray());
        if (selected is null || !choices.TryGetValue(selected, out var position)
            || session.Document?.Id != document.Id || session.Busy)
            return;

        StopSpeech();
        await session.SelectSentenceAsync(position);
    }

    private static string BookmarkLabel(int position, string text)
    {
        var preview = string.Join(' ', text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
        var elements = new System.Globalization.StringInfo(preview);
        if (elements.LengthInTextElements > 48)
            preview = elements.SubstringByTextElements(0, 48) + "…";
        return $"{position + 1}. {preview}";
    }

    private async void PreviousClicked(object? sender, EventArgs args)
    {
        StopSpeech(); await AppState.Session.MoveAsync(-1);
    }
    private async void NextClicked(object? sender, EventArgs args)
    {
        StopSpeech(); await AppState.Session.MoveAsync(1);
    }
    private async void SentenceChanged(object? sender, EventArgs args)
    {
        if (SentencePicker.SelectedIndex < 0 || AppState.Session.Busy) return;
        StopSpeech(); await AppState.Session.SelectSentenceAsync(SentencePicker.SelectedIndex);
    }

    private async void SpeakClicked(object? sender, EventArgs args)
    {
        if (AppState.Session.Sentence is not { } sentence || AppState.Session.Document is not { } doc) return;
        var request = ++speechRequest;
        speech.Rate = Rate.SelectedIndex switch { 0 => .75f, 2 => 1.25f, _ => 1f };
        SpeechStatus.Text = "Озвучення поточного речення…";
        var status = await speech.SpeakAsync(sentence.Text, doc.Language);
        if (request == speechRequest && status is not null) SpeechStatus.Text = status;
    }
    private void StopClicked(object? sender, EventArgs args)
    {
        StopSpeech();
    }

    private void StopSpeech()
    {
        speechRequest++;
        speech.Stop();
        SpeechStatus.Text = "Озвучення зупинено.";
    }
}
