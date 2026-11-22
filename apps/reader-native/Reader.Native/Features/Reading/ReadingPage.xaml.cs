using System.ComponentModel;
using System.Diagnostics;
using Reader.Contracts;
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
    private bool translating;
    private (string Document, string Sentence, string Target, string Text)? machineKey;
    private Reader.Contracts.MachineTranslation? machineTranslation;
    private string? machineStatus;
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
    private LemmaSnapshot[] vocabularyReadings = [];
    private string? documentId;
    private int speechRequest;
    private bool speechPaused;
    private bool utteranceActive;
    private (string DocumentId, SpeechResume Resume)? pendingSpeechResume;
    private long lastSpeechSave;
    private int? resumeSpeechOffset;
    private bool loadingSpeechPreferences;
    private bool speechPreferencesReady;
    private const string RatePreference = "reader.speech.rate";
    private const string ContinuousPreference = "reader.speech.continuous";
    private SpeechVoice[] availableVoices = [];
    private string? voiceLanguage;
    public ReadingPage() => InitializeComponent();

    protected override void OnAppearing()
    {
        base.OnAppearing();
        BindingContext = AppState.Session;
        AppState.Session.PropertyChanged += SessionChanged;
        UpdateHighlight();
        UpdateTranslation();
        RefreshSpeechResume();
        loadingSpeechPreferences = true;
        try
        {
            Rate.SelectedIndex = Math.Clamp(Preferences.Get(RatePreference, 1), 0, 2);
            ContinueSpeech.IsChecked = Preferences.Get(ContinuousPreference, false);
        }
        finally { loadingSpeechPreferences = false; }
        speechPreferencesReady = true;
        RefreshVoices(force: true);
    }

    protected override void OnDisappearing()
    {
        StopSpeech();
        selection = null;
        WordCard.IsVisible = false;
        Translation.Text = "";
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
            RefreshVoices();
            if (!utteranceActive)
            {
                SaveSpeechResume();
                RefreshSpeechResume();
            }
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
        var key = ActiveTranslationKey();
        var machine = key is not null && key == machineKey ? machineTranslation : null;
        TranslationNotice.Text = !active ? "Відкрийте документ для читання."
            : text is not null ? "Ваш переклад активного речення"
            : machine?.PivotLanguage is { } pivot ? $"Машинний переклад через {pivot}"
            : machine is not null ? "Машинний переклад"
            : key == machineKey && machineStatus is not null ? machineStatus
            : "Перекладу ще немає.";
        SegmentTranslation.Text = text ?? machine?.Text ?? "";
        EditTranslation.Text = text is null ? "Додати свій переклад" : "Змінити переклад";
        EditTranslation.IsEnabled = active && !session.Busy && !translating;
        TranslateOffline.IsEnabled = active && !session.Busy && !translating && text is null;
        TargetLanguage.IsEnabled = active && !session.Busy;
    }

    private (string Document, string Sentence, string Target, string Text)? ActiveTranslationKey()
    {
        var session = AppState.Session;
        return session.Document is { } document && session.Sentence is { } sentence && TargetCode is { } target
            ? (document.Id, sentence.Id, target, sentence.Text)
            : null;
    }

    private async void TranslateOfflineClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (translating || session.Busy || ActiveTranslationKey() is not { } key) return;
        translating = true;
        machineKey = key;
        machineTranslation = null;
        machineStatus = "Переклад виконується…";
        UpdateTranslation();
        try
        {
            machineTranslation = await session.TranslateSentenceAsync(key.Document, key.Sentence, key.Target);
            machineStatus = machineTranslation is null ? "Для цієї пари не встановлено мовний пакет." : null;
        }
        catch (Exception error) { machineStatus = error.Message; }
        finally
        {
            translating = false;
            UpdateTranslation();
        }
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
            accept: "Зберегти", cancel: "Скасувати", maxLength: 4000,
            initialValue: existing ?? (ActiveTranslationKey() == machineKey ? machineTranslation?.Text : null) ?? "");
        if (text is not null && session.Document?.Id == document.Id && session.Sentence?.Id == sentence.Id
            && TargetCode == target && !session.Busy)
            await session.SetReviewedTranslationAsync(document.Id, sentence.Id, target, text);
    }

    private async void SourceSelected(object? sender, TextSelection selected)
    {
        var session = AppState.Session;
        if (session.Document is not { } doc) return;
        if (selection != selected) Translation.Text = "";
        selection = selected;
        vocabularyReadings = [];
        VocabularyReading.IsVisible = false;
        SelectedWord.Text = selected.Text;
        KnowledgeAnalysis.Text = "Пошук у мовному пакеті…";
        WordCard.IsVisible = true;
        var index = Array.FindIndex(doc.Sentences, sentence => selected.StartUtf16 >= sentence.StartUtf16
            && selected.StartUtf16 < sentence.EndUtf16);
        if (index >= 0 && index != doc.Position) { StopSpeech(); await session.SelectSentenceAsync(index); }
        try
        {
            var lookup = await session.LookupWordAsync(doc.Language, selected.Text);
            if (selection != selected || session.Document?.Id != doc.Id || !WordCard.IsVisible) return;
            if (lookup is null)
            {
                KnowledgeAnalysis.Text = "Пакет граматичного аналізу для цієї мови відсутній.";
                return;
            }
            var source = $"{lookup.Package.Name} {lookup.Package.Version} · {lookup.Package.License}";
            var readings = lookup.Analyses.Select(analysis =>
                $"{analysis.Lemma} · {analysis.PartOfSpeech}" +
                (analysis.Features.Length == 0 ? "" : $" · {analysis.Features.Replace("|", ", ")}") +
                $" · {(double)analysis.Observations / lookup.TotalObservations:P0}");
            KnowledgeAnalysis.Text = lookup.Analyses.Length == 0
                ? $"Словоформи немає в корпусі {source}."
                : $"{string.Join(Environment.NewLine, readings)}{Environment.NewLine}Частота в корпусі · {source}";
            vocabularyReadings = lookup.Analyses.DistinctBy(analysis => (analysis.Lemma, analysis.PartOfSpeech))
                .Select(analysis => new LemmaSnapshot(analysis.Lemma, selected.Text, analysis.PartOfSpeech,
                    analysis.Features, lookup.Package.Name, $"{lookup.Package.Version} · {lookup.Package.Revision}"))
                .ToArray();
            if (vocabularyReadings.Length > 0)
            {
                VocabularyReading.ItemsSource = new[] { $"Виділена форма · {selected.Text}" }
                    .Concat(vocabularyReadings.Select(analysis => $"Лема · {analysis.Lemma} · {analysis.PartOfSpeech}")).ToArray();
                VocabularyReading.SelectedIndex = vocabularyReadings.Length == 1 ? 1 : 0;
                VocabularyReading.IsVisible = true;
            }
        }
        catch (Exception error)
        {
            if (selection == selected && session.Document?.Id == doc.Id && WordCard.IsVisible)
                KnowledgeAnalysis.Text = $"Не вдалося виконати аналіз. {error.Message}";
        }
    }

    private async void SaveWordClicked(object? sender, EventArgs args)
    {
        if (selection is not { } selected) return;
        var index = VocabularyReading.SelectedIndex - 1;
        var analysis = VocabularyReading.IsVisible && index >= 0 && index < vocabularyReadings.Length
            ? vocabularyReadings[index] : null;
        await AppState.Session.SaveWordAsync(selected, Translation.Text ?? "", analysis);
        if (selection == selected && AppState.Session.Error.Length == 0)
        {
            WordCard.IsVisible = false;
            Translation.Text = "";
        }
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

    private async void ViewOriginalClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (!session.CanViewOriginal || session.Document is not { } document) return;
        try
        {
            var original = await session.OriginalAsync(document.Id);
            if (session.Document?.Id != document.Id) return;
            var bytes = original.Bytes.Select(value => checked((byte)value)).ToArray();
            var isPdf = document.Format == "pdf";
            var total = isPdf ? await PdfScan.PageCountAsync(bytes) : 1;
            var initial = Math.Clamp(session.OriginalPageIndex(), 0, total - 1);
            if (session.Document?.Id != document.Id) return;
            await Navigation.PushAsync(new OriginalPage(original.Name, total, initial, isPdf,
                page => isPdf ? PdfScan.PreviewAsync(bytes, page) : PhotoOcr.PreviewAsync(bytes),
                document.Id, session, document.OcrLines ?? []));
        }
        catch (Exception error) { await DisplayAlertAsync("Оригінал", error.Message, "Гаразд"); }
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
        if (SentencePicker.SelectedIndex < 0 || AppState.Session.Busy ||
            SentencePicker.SelectedIndex == AppState.Session.Document?.Position) return;
        StopSpeech(); await AppState.Session.SelectSentenceAsync(SentencePicker.SelectedIndex);
    }

    private static string VoicePreference(string language) => $"reader.speech.voice.{language}";

    private void RefreshVoices(bool force = false)
    {
        var language = AppState.Session.Document?.Language;
        if (!force && voiceLanguage == language) return;
        voiceLanguage = language;
        loadingSpeechPreferences = true;
        try
        {
            availableVoices = language is null ? [] : LocalSpeech.VoicesForLanguage(language);
            VoicePicker.ItemsSource = new[] { "Системний вибір" }.Concat(availableVoices.Select(voice =>
                $"{voice.Name} ({voice.Language})")).ToArray();
            var saved = language is null ? "" : Preferences.Get(VoicePreference(language), "");
            var chosen = Array.FindIndex(availableVoices, voice => voice.Id == saved);
            VoicePicker.SelectedIndex = chosen + 1;
            VoicePicker.IsEnabled = availableVoices.Length > 0;
            var status = language is null ? ""
                : availableVoices.Length == 0 ? "Для цієї мови немає встановленого голосу."
                : chosen < 0 && saved.Length > 0 ? "Збережений голос недоступний. Використано системний вибір." : "";
            SetVoiceStatus(status);
        }
        catch (Exception error)
        {
            availableVoices = [];
            VoicePicker.ItemsSource = Array.Empty<string>();
            VoicePicker.IsEnabled = false;
            SetVoiceStatus($"Не вдалося отримати список голосів: {error.Message}");
        }
        finally { loadingSpeechPreferences = false; }
    }

    private void SetVoiceStatus(string status)
    {
        VoiceStatus.Text = status;
        VoiceStatus.IsVisible = status.Length > 0;
    }

    private void VoiceChanged(object? sender, EventArgs args)
    {
        if (loadingSpeechPreferences || voiceLanguage is null || VoicePicker.SelectedIndex < 0) return;
        Preferences.Set(VoicePreference(voiceLanguage), SelectedVoiceId() ?? "");
        SetVoiceStatus("");
    }

    private string? SelectedVoiceId() => VoicePicker.SelectedIndex > 0 && VoicePicker.SelectedIndex <= availableVoices.Length
        ? availableVoices[VoicePicker.SelectedIndex - 1].Id : null;

    private void SpeechPreferencesChanged(object? sender, EventArgs args)
    {
        if (!speechPreferencesReady || loadingSpeechPreferences || Rate.SelectedIndex < 0) return;
        Preferences.Set(RatePreference, Rate.SelectedIndex);
        Preferences.Set(ContinuousPreference, ContinueSpeech.IsChecked);
    }

    private void RefreshSpeechResume()
    {
        resumeSpeechOffset = null;
        ResumeSpeech.IsVisible = false;
        var session = AppState.Session;
        if (session.Busy || session.Document is not { } document || session.Sentence is not { } sentence) return;

        try
        {
            resumeSpeechOffset = SpeechProgress.ResumeOffset(sentence, session.GetSpeechResume(document.Id));
            ResumeSpeech.IsVisible = resumeSpeechOffset.HasValue;
        }
        catch (Exception error) { SpeechStatus.Text = $"Не вдалося прочитати позицію озвучення. {error.Message}"; }
    }

    private void SaveSpeechResume()
    {
        var session = AppState.Session;
        if (pendingSpeechResume is not { } pending || session.Busy) return;
        if (session.Document?.Id != pending.DocumentId || session.Sentence is not { } sentence ||
            sentence.Id != pending.Resume.SentenceId || SpeechProgress.TextHash(sentence) != pending.Resume.TextHash)
        {
            pendingSpeechResume = null;
            return;
        }
        try
        {
            var resume = pending.Resume;
            session.SetSpeechResume(pending.DocumentId, resume.SentenceId, resume.TextHash, resume.OffsetUtf16);
            pendingSpeechResume = null;
            lastSpeechSave = Stopwatch.GetTimestamp();
        }
        catch (Exception error) { SpeechStatus.Text = $"Не вдалося зберегти позицію озвучення. {error.Message}"; }
    }

    private async void SpeakClicked(object? sender, EventArgs args) => await SpeakAsync(0);

    private async void ResumeSpeechClicked(object? sender, EventArgs args)
    {
        RefreshSpeechResume();
        if (resumeSpeechOffset is int offset) await SpeakAsync(offset);
    }

    private async Task SpeakAsync(int offset)
    {
        var session = AppState.Session;
        if (session.Busy || session.Document is not { } initial || session.Sentence is null) return;
        SaveSpeechResume();
        var request = ++speechRequest;
        var receivedCue = false;
        var status = LocalSpeech.CompletedStatus;
        RefreshVoices(force: true);
        speech.PreferredVoice = SelectedVoiceId();
        speech.Rate = Rate.SelectedIndex switch { 0 => .75f, 2 => 1.25f, _ => 1f };

        while (request == speechRequest && session.Document is { } doc && doc.Id == initial.Id &&
            session.Sentence is { } sentence)
        {
            var startAt = offset;
            offset = 0;
            var textHash = SpeechProgress.TextHash(sentence);
            lastSpeechSave = 0;
            SourceText.SpokenWord = null;
            speechPaused = false;
            utteranceActive = true;
            ResumeSpeech.IsVisible = false;
            PauseSpeech.Text = "Пауза";
            PauseSpeech.IsEnabled = true;
            SpeechStatus.Text = "Озвучення поточного речення…";
            status = await speech.SpeakAsync(sentence.Text[startAt..], doc.Language, (start, length) => Dispatcher.Dispatch(() =>
            {
                if (request == speechRequest && utteranceActive && !speechPaused &&
                    session.Document?.Id == doc.Id && session.Sentence?.Id == sentence.Id &&
                    session.Sentence?.Text == sentence.Text &&
                    HighlightSpokenWord(sentence, startAt + start, length) is int spokenOffset)
                {
                    receivedCue = true;
                    pendingSpeechResume = (doc.Id, new(sentence.Id, textHash, spokenOffset));
                    if (lastSpeechSave == 0 || Stopwatch.GetElapsedTime(lastSpeechSave) >= TimeSpan.FromSeconds(2))
                        SaveSpeechResume();
                }
            }));
            if (request != speechRequest) return;
            utteranceActive = false;
            SourceText.SpokenWord = null;
            PauseSpeech.IsEnabled = false;
            if (status == LocalSpeech.CompletedStatus)
            {
                pendingSpeechResume = null;
                try { session.SetSpeechResume(doc.Id, sentence.Id, textHash, null); }
                catch (Exception error)
                {
                    status = $"Не вдалося очистити позицію озвучення. {error.Message}";
                }
            }
            else SaveSpeechResume();
            if (status != LocalSpeech.CompletedStatus || !ContinueSpeech.IsChecked ||
                doc.Position + 1 >= doc.Sentences.Length || session.Busy)
                break;

            await session.SelectSentenceAsync(doc.Position + 1);
            if (request != speechRequest) return;
            if (session.Error.Length > 0 || session.Document?.Position != doc.Position + 1)
            {
                status = session.Error.Length > 0 ? session.Error : "Не вдалося перейти до наступного речення.";
                break;
            }
        }
        if (request != speechRequest) return;
        // Invalidate any late cue posted by an engine just before completion.
        speechRequest++;
        SourceText.SpokenWord = null;
        PauseSpeech.IsEnabled = false;
        if (status is not null)
            SpeechStatus.Text = status == LocalSpeech.CompletedStatus && !receivedCue
                ? $"{status} Межі слів від голосу не отримано." : status;
        RefreshSpeechResume();
    }

    private int? HighlightSpokenWord(Sentence sentence, int start, int length)
    {
        var span = SourceRanges.Selection(sentence.Text, start, length);
        if (span is null) return null;
        var absolute = sentence.StartUtf16 + span.StartUtf16;
        if (absolute < 0 || absolute > SourceText.Text.Length - span.LengthUtf16 ||
            !SourceText.Text.AsSpan(absolute, span.LengthUtf16).SequenceEqual(span.Text)) return null;
        SourceText.SpokenWord = span with { StartUtf16 = absolute };
        return span.StartUtf16;
    }

    private void PauseClicked(object? sender, EventArgs args)
    {
        if (speechPaused ? !speech.Resume() : !speech.Pause()) return;
        speechPaused = !speechPaused;
        PauseSpeech.Text = speechPaused ? "Продовжити" : "Пауза";
        SpeechStatus.Text = speechPaused ? "Озвучення призупинено." : "Озвучення поточного речення…";
        if (speechPaused) SaveSpeechResume();
    }

    private void StopClicked(object? sender, EventArgs args) => StopSpeech();

    private void StopSpeech()
    {
        speechRequest++;
        speech.Stop();
        speechPaused = false;
        utteranceActive = false;
        SourceText.SpokenWord = null;
        PauseSpeech.IsEnabled = false;
        PauseSpeech.Text = "Пауза";
        SpeechStatus.Text = "Озвучення зупинено.";
        SaveSpeechResume();
        RefreshSpeechResume();
    }
}
