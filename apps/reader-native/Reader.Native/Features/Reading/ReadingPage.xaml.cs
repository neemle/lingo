using System.ComponentModel;
using System.Diagnostics;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage : ContentPage
{
    private (string Code, string Name)[] availableTargets = [];
    private string? lastSourceLanguage;
    private bool updatingTargets;
    private bool narrowReader;
    private ReaderLayout readingLayout;
    private bool TranslationVisible => readingLayout != ReaderLayout.Source;
    private const string LayoutPreference = "reader.layout";
    private enum ReaderLayout { Automatic, Columns, Stacked, Source, Translation }
    private static readonly (ReaderLayout Mode, string Label, string Compact)[] Layouts =
    [
        (ReaderLayout.Automatic, "Автоматично", "Авто"),
        (ReaderLayout.Columns, "Поруч", "Поруч"),
        (ReaderLayout.Stacked, "Один під одним", "Вертикально"),
        (ReaderLayout.Source, "Лише оригінал", "Оригінал"),
        (ReaderLayout.Translation, "Лише переклад", "Переклад"),
    ];
    private bool translating;
    private TranslationKey? statusKey;
    private string? machineStatus;
    private (string Source, string Target)? readinessPair;
    private TranslationReadiness? readiness;
    private string? readinessError;
    private int readinessRequest;
    private const string TargetPreferencePrefix = "reader.translation.target.";
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
    private sealed record VocabularyReadingOption(string Label, LemmaSnapshot? Lemma = null,
        ExpressionSnapshot? Expression = null);
    private VocabularyReadingOption[] vocabularyReadings = [];
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
    public ReadingPage() => InitializeComponent();

    protected override void OnAppearing()
    {
        base.OnAppearing();
        var storedLayout = (ReaderLayout)Preferences.Get(LayoutPreference, (int)ReaderLayout.Automatic);
        readingLayout = Enum.IsDefined(storedLayout) ? storedLayout : ReaderLayout.Automatic;
        ArrangeReadingColumns();
        readingPageActive = true;
        autoTranslate = Preferences.Get(AutoTranslatePreference, false);
        lastAutoTranslation = null;
        UpdateAutoTranslateButton();
        BindingContext = AppState.Session;
        AppState.Session.PropertyChanged += SessionChanged;
        BeginReadingHistory();
        UpdateHighlight();
        readinessPair = null;
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
        readingPageActive = false;
        CancelScheduledTranslation();
        EndReadingHistory(keepVisit: showingMeanings);
        StopSpeech();
        voiceCatalogRevision++;
        readinessRequest++;
        if (!showingMeanings)
        {
            selection = null;
            SetSelectedSense(null);
            WordCard.IsVisible = false;
            Translation.Text = "";
        }
        AppState.Session.PropertyChanged -= SessionChanged;
        base.OnDisappearing();
    }

    private void SessionChanged(object? sender, PropertyChangedEventArgs args)
    {
        // Notifications describe a completed session state. Update once per state change.
        if (args.PropertyName == nameof(ReaderSession.Snapshot))
        {
            SyncReadingHistory();
            UpdateHighlight();
            UpdateTranslation();
            RefreshVoices();
            if (activeSpeechSource is { } source && !IsSpeechSourceCurrent(AppState.Session, source))
                StopSpeech();
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
        var document = AppState.Session.Document;
        var sentence = AppState.Session.Sentence;
        PlayerProgress.Progress = document is { Sentences.Length: > 0 }
            ? (double)(document.Position + 1) / document.Sentences.Length : 0;
        SourceLanguageBadge.IsVisible = document is not null;
        SourceLanguageCode.Text = document?.Language.ToUpperInvariant() ?? "";
        SourceLanguageName.Text = document is null ? "" : DocumentLanguages.Name(document.Language);
        SourceText.HighlightLength = sentence is null ? 0 : sentence.EndUtf16 - sentence.StartUtf16;
        if (documentId == AppState.Session.Document?.Id) return;
        documentId = AppState.Session.Document?.Id;
        selection = null;
        SetSelectedSense(null);
        WordCard.IsVisible = false;
        Translation.Text = "";
    }

    protected override void OnSizeAllocated(double width, double height)
    {
        base.OnSizeAllocated(width, height);
        if (ReadingColumns is null || SourcePane is null || TranslationPane is null ||
            ReaderLayoutButton is null || SourceHint is null || width <= 0 || narrowReader == (width < 720)) return;
        narrowReader = width < 720;
        ArrangeReadingColumns();
    }

    private void ArrangeReadingColumns()
    {
        var sourceVisible = readingLayout != ReaderLayout.Translation;
        var split = sourceVisible && TranslationVisible;
        var stacked = split && (readingLayout == ReaderLayout.Stacked
            || readingLayout == ReaderLayout.Automatic && narrowReader);
        var columns = split && !stacked;
        SourcePane.IsVisible = SourceHint.IsVisible = sourceVisible;
        TranslationPane.IsVisible = TranslationVisible;
        ReadingColumns.ColumnDefinitions[1].Width = columns ? GridLength.Star : 0;
        ReadingColumns.RowDefinitions[1].Height = stacked ? GridLength.Star : 0;
        ReadingColumns.ColumnSpacing = columns ? 1 : 0;
        ReadingColumns.RowSpacing = stacked ? 1 : 0;
        Grid.SetColumn(TranslationPane, columns ? 1 : 0);
        Grid.SetRow(TranslationPane, stacked ? 1 : 0);
        var layout = Layouts.First(item => item.Mode == readingLayout);
        ReaderLayoutButton.Text = layout.Compact;
        SemanticProperties.SetDescription(ReaderLayoutButton, $"Розкладка читання · {layout.Label}. Змінити розкладку");
    }

    private async void ReaderLayoutClicked(object? sender, EventArgs args)
    {
        ReaderLayoutButton.IsEnabled = false;
        try
        {
            var choice = await DisplayActionSheetAsync("Розкладка читання", "Скасувати", null,
                Layouts.Select(layout => layout.Label).ToArray());
            var index = Array.FindIndex(Layouts, layout => layout.Label == choice);
            if (index < 0 || readingLayout == Layouts[index].Mode) return;
            readingLayout = Layouts[index].Mode;
            Preferences.Set(LayoutPreference, (int)readingLayout);
            ArrangeReadingColumns();
            ScheduleAutoTranslation();
        }
        finally { ReaderLayoutButton.IsEnabled = true; }
    }

    private void DecreaseReaderFont(object? sender, EventArgs args) => ChangeReadingSize(-1);
    private void IncreaseReaderFont(object? sender, EventArgs args) => ChangeReadingSize(1);

    private void ChangeReadingSize(int change)
    {
        var size = Math.Clamp(SourceText.ReadingFontSize + change, 18, 30);
        SourceText.ReadingFontSize = size;
        SegmentTranslation.FontSize = size - 2;
        UpdateTranslation();
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
                availableTargets = DocumentLanguages.All.Where(language => language.Code != source).ToArray();
                TargetLanguage.ItemsSource = availableTargets.Select(language => language.Name).ToArray();
                var preferred = source is null ? previous : Preferences.Get(TargetPreferencePrefix + source, previous ?? "");
                var preferredIndex = Array.FindIndex(availableTargets, language => language.Code == preferred);
                var defaultIndex = Array.FindIndex(availableTargets, language => language.Code == (source == "uk" ? "fr" : "uk"));
                TargetLanguage.SelectedIndex = preferredIndex >= 0 ? preferredIndex : defaultIndex;
            }
            finally { updatingTargets = false; }
        }

        var active = session.Sentence is not null && TargetCode is not null;
        var text = TargetCode is { } target ? session.ReviewedTranslation(target) : null;
        var key = ActiveTranslationKey();
        if (key is { } current && readinessPair != (current.Source, current.Target))
            RefreshReadiness((current.Source, current.Target));
        var document = session.Document;
        var window = active && document is not null && TargetCode is { } targetCode
            ? FindTranslationWindow(document, targetCode) : null;
        var unit = document is not null && window is not null ? window.UnitFor(document.Position) : null;
        var alignment = active && document is not null && TargetCode is { } machineTarget
            ? ActiveMachineAlignment(document, machineTarget) : null;
        var machine = alignment?.Translation;
        var machineLabel = alignment?.Unit.Sources.Length > 1 ? "Машинний переклад групи речень"
            : machine?.JoinedLines == true ? "Машинний переклад об’єднаних рядків" : "Машинний переклад";
        TranslationNotice.Text = !active ? "Відкрийте документ для читання."
            : translating && key == statusKey ? machineStatus
            : text is not null ? "Ваш переклад активного речення"
            : machine?.PivotLanguage is { } pivot ? $"{machineLabel} через {DocumentLanguages.Name(pivot)}"
            : machine is not null ? machineLabel
            : key == statusKey && machineStatus is not null ? machineStatus
            : ReadinessMessage();
        SegmentTranslation.Text = text ?? machine?.Text ?? "";
        ActiveTranslationCard.IsVisible = SegmentTranslation.Text.Length > 0;
        ShareTranslationPair.IsVisible = DisplayedTranslationPair() is not null;
        EditTranslation.Text = text is null ? "Додати свій переклад" : "Змінити переклад";
        EditTranslation.IsEnabled = active && !session.Busy && !translating;
        var canExtend = document is not null && window is not null && document.Position != window.Anchor
            && (window.MoreBefore || window.MoreAfter);
        TranslateOffline.IsEnabled = active && !session.Busy && !translating && readiness?.Ready == true
            && (window is null || !window.IsParagraph || canExtend
                || alignment is not null && !ReferenceEquals(unit, alignment.Value.Unit)
                || document is not null && HasUntranslatedUnits(document, window));
        TranslateSentence.IsEnabled = active && !session.Busy && !translating && readiness?.Ready == true && text is null;
        OpenTranslationPackages.IsVisible = active && readiness is { Missing.Length: > 0 };
        TargetLanguage.IsEnabled = active && !session.Busy;
        UpdateSourceLocation(document, session.Sentence);
        UpdateParagraphPreview(document, window, unit);
        ScheduleAutoTranslation();
    }

    private string ReadinessMessage()
    {
        if (readinessError is not null) return readinessError;
        if (readiness is null) return "Перевіряємо мовні пакети…";
        if (readiness.Ready)
            return readiness.PivotLanguage is { } pivot
                ? $"Доступний офлайн-переклад через {DocumentLanguages.Name(pivot)}."
                : "Доступний прямий офлайн-переклад.";
        if (readiness.Missing.Length == 0) return "Для цієї пари в каталозі немає офлайн-моделі.";
        var pairs = readiness.Missing.Select(pair => $"{DocumentLanguages.Name(pair.Source)} → {DocumentLanguages.Name(pair.Target)}");
        return $"Потрібні мовні пакети · {string.Join(", ", pairs)}";
    }

    private async void RefreshReadiness((string Source, string Target) pair)
    {
        var request = ++readinessRequest;
        readinessPair = pair;
        readiness = null;
        readinessError = null;
        try
        {
            var status = await AppState.Session.TranslationReadinessAsync(pair.Source, pair.Target);
            if (request != readinessRequest) return;
            readiness = status;
        }
        catch (Exception error)
        {
            if (request != readinessRequest) return;
            readinessError = error.Message;
        }
        UpdateTranslation();
    }

    private async void OpenTranslationPackagesClicked(object? sender, EventArgs args)
    {
        if (readiness is not { Missing.Length: > 0 } state) return;
        try
        {
            await Shell.Current.GoToAsync("//settings",
                new ShellNavigationQueryParameters { ["translationPackages"] = state.Missing });
        }
        catch (Exception error) { TranslationNotice.Text = error.Message; }
    }

    private TranslationKey? ActiveTranslationKey()
    {
        var session = AppState.Session;
        return session.Document is { } document && session.Sentence is { } sentence && TargetCode is { } target
            ? new TranslationKey(document.Id, sentence.Id, document.Language, target, sentence.Text)
            : null;
    }

    private void TargetChanged(object? sender, EventArgs args)
    {
        if (TranslationNotice is null || updatingTargets) return;
        if (AppState.Session.Document is { } document && TargetCode is { } target)
            Preferences.Set(TargetPreferencePrefix + document.Language, target);
        UpdateTranslation();
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
            initialValue: existing ?? IndependentMachineText(document, target) ?? "");
        if (text is not null && session.Document?.Id == document.Id && session.Sentence?.Id == sentence.Id
            && TargetCode == target && !session.Busy)
            await session.SetReviewedTranslationAsync(document.Id, sentence.Id, target, text);
    }

    private async void SourceSelected(object? sender, TextSelection selected)
    {
        var session = AppState.Session;
        if (session.Document is not { } doc) return;
        RecordReadingActivity();
        if (selection != selected)
        {
            Translation.Text = "";
            SetSelectedSense(null);
        }
        WordExplanationText.Text = "";
        WordExplanationText.IsVisible = false;
        selection = selected;
        vocabularyReadings = [];
        VocabularyReading.IsVisible = false;
        SelectedWord.Text = selected.Text;
        ShowMeanings(null);
        ShowLocalSyntax(null);
        ShowSpelling(null);
        KnowledgeAnalysis.Text = "Пошук у мовному пакеті…";
        WordCard.IsVisible = true;
        var index = Array.FindIndex(doc.Sentences, sentence => selected.StartUtf16 >= sentence.StartUtf16
            && selected.StartUtf16 < sentence.EndUtf16);
        if (index >= 0 && index != doc.Position) { StopSpeech(); await session.SelectSentenceAsync(index); }
        try
        {
            var context = index < 0 ? null : new WordContext(doc.Sentences[index].Text,
                selected.StartUtf16 - doc.Sentences[index].StartUtf16);
            var lookup = await session.LookupWordAsync(doc.Language, selected.Text, context);
            if (selection != selected || session.Document is not { } current || current.Id != doc.Id
                || current.Language != doc.Language || !WordCard.IsVisible) return;
            ShowMeanings(lookup);
            ShowLocalSyntax(lookup);
            ShowSpelling(lookup);
            if (lookup is null)
            {
                KnowledgeAnalysis.Text = "Встановіть мовний пакет у налаштуваннях.";
                return;
            }
            var source = $"{lookup.Package.Name} {lookup.Package.Version} · {lookup.Package.License}";
            var readings = lookup.Analyses.Select(analysis =>
                $"{analysis.Lemma} · {analysis.PartOfSpeech}" +
                (analysis.Features.Length == 0 ? "" : $" · {analysis.Features.Replace("|", ", ")}") +
                (analysis.GrammarEvidence switch
                {
                    "frenchDeterminerGender" => " · рід узгоджується з артиклем",
                    "ukrainianGenitivePreposition" => " · прийменник вимагає родового відмінка",
                    "ukrainianInfinitiveComplement" => " · інфінітив після дієслова «хотіти»",
                    "englishDeterminerNoun" => " · іменникове читання після артикля",
                    _ => "",
                }) +
                $" · {(double)analysis.Observations / lookup.TotalObservations:P0}" +
                (analysis.ContextObservations == 0 ? "" : $" · збігів із сусідніми словами: {analysis.ContextObservations}"));
            KnowledgeAnalysis.Text = lookup.Analyses.Length == 0
                ? $"Словоформи немає в корпусі {source}."
                : $"{string.Join(Environment.NewLine, readings)}{Environment.NewLine}Частота в корпусі · {source}";
            if (lookup.Expressions.Length > 0)
                KnowledgeAnalysis.Text += Environment.NewLine + string.Join(Environment.NewLine, lookup.Expressions.Select(expression =>
                    $"{(expression.Kind == "particle" ? "Дієслово з часткою" : "Сталий вираз")} · {expression.Text} · у корпусі: {expression.Observations}"));
            var version = $"{lookup.Package.Version} · {lookup.Package.Revision}";
            var lemmas = lookup.Analyses.DistinctBy(analysis => (analysis.Lemma, analysis.PartOfSpeech))
                .Select(analysis => new VocabularyReadingOption($"Лема · {analysis.Lemma} · {analysis.PartOfSpeech}",
                    Lemma: new LemmaSnapshot(analysis.Lemma, selected.Text, analysis.PartOfSpeech,
                        analysis.Features, lookup.Package.Name, version)));
            var expressions = lookup.Expressions.DistinctBy(expression => (expression.Canonical, expression.Kind))
                .Select(expression => new VocabularyReadingOption($"Вираз · {expression.Text}",
                    Expression: new ExpressionSnapshot(expression.Canonical, expression.Text, expression.Kind,
                        expression.Observations, lookup.Package.Name, version)));
            vocabularyReadings = lemmas.Concat(expressions)
                .ToArray();
            if (vocabularyReadings.Length > 0)
            {
                VocabularyReading.ItemsSource = new[] { $"Виділена форма · {selected.Text}" }
                    .Concat(vocabularyReadings.Select(reading => reading.Label)).ToArray();
                VocabularyReading.SelectedIndex = vocabularyReadings is [{ Lemma: not null }] ? 1 : 0;
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
        var reading = VocabularyReading.IsVisible && index >= 0 && index < vocabularyReadings.Length
            ? vocabularyReadings[index] : null;
        if (selectedSense is not null && reading?.Expression is not null)
        {
            await DisplayAlertAsync("Словникова картка", "Зніміть вибір значення або виберіть слово чи лему.",
                "Гаразд");
            return;
        }
        var lemma = reading?.Lemma;
        if (selectedSense is { } sense && lemma is not null &&
            (!string.Equals(lemma.Lemma.Trim().Normalize(), sense.Lemma.Normalize(), StringComparison.OrdinalIgnoreCase) ||
             lemma.PartOfSpeech != sense.PartOfSpeech))
            lemma = null;
        await AppState.Session.SaveWordAsync(selected, Translation.Text ?? "", lemma, reading?.Expression, selectedSense);
        if (selection == selected && AppState.Session.Error.Length == 0)
        {
            WordCard.IsVisible = false;
            Translation.Text = "";
            SetSelectedSense(null);
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
                document.Id, session, document.OcrLines ?? [], selection));
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
        if (session.Busy || SelectedVoice()?.Engine is not null ||
            session.Document is not { } document || session.Sentence is not { } sentence) return;

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
        if (session.Busy || session.Document is not { } initial || session.Sentence is not { } sentence) return;
        var source = SpeechSourceFor(initial, sentence);
        SaveSpeechResume();
        var request = ++speechRequest;
        speech.Stop();
        CancelModelSpeech();
        speechPaused = false;
        utteranceActive = false;
        activeSpeechSource = null;
        PauseSpeech.IsEnabled = false;

        await loadingVoices;
        if (request != speechRequest || !IsSpeechSourceCurrent(session, source) || session.Busy) return;
        using var run = new CancellationTokenSource();
        modelSpeechRun = run;
        try { await SpeakRunAsync(session, initial.Id, offset, request, run.Token); }
        finally
        {
            if (ReferenceEquals(modelSpeechRun, run)) CancelModelSpeech();
        }
    }

    private async Task SpeakRunAsync(ReaderSession session, string documentId, int offset, int request, CancellationToken token)
    {
        var voice = SelectedVoice();
        var modelVoice = voice?.Engine is not null;
        var rate = Rate.SelectedIndex switch { 0 => .75f, 2 => 1.25f, _ => 1f };
        speech.PreferredVoice = modelVoice ? null : voice?.Id;
        speech.Rate = rate;
        var receivedCue = false;
        string? status = LocalSpeech.CompletedStatus;

        while (request == speechRequest && session.Document is { } doc && doc.Id == documentId &&
            session.Sentence is { } sentence)
        {
            var startAt = modelVoice ? 0 : offset;
            offset = 0;
            var textHash = SpeechProgress.TextHash(sentence);
            lastSpeechSave = 0;
            SourceText.SpokenWord = null;
            speechPaused = false;
            utteranceActive = true;
            activeSpeechSource = SpeechSourceFor(doc, sentence);
            ResumeSpeech.IsVisible = false;
            PauseSpeech.Text = "Пауза";
            PauseSpeech.IsEnabled = !modelVoice;
            SpeechStatus.Text = modelVoice ? "Готуємо локальне озвучення…" : "Озвучення поточного речення…";

            RecordReadingActivity();
            if (modelVoice)
            {
                status = await PlayModelAsync(session, doc, sentence, voice!, rate, request, token);
                if (request != speechRequest) return;
                if (status is not null && status != LocalSpeech.CompletedStatus)
                {
                    CancelModelSpeech();
                    modelVoice = false;
                    SetVoiceStatus("Локальний голос недоступний. Використовуємо системний голос.");
                    PauseSpeech.IsEnabled = true;
                    SpeechStatus.Text = "Озвучення поточного речення…";
                }
            }
            if (!modelVoice)
            {
                var fallbackReason = status != LocalSpeech.CompletedStatus ? status : null;
                status = await PlaySystemSentenceAsync(session, doc, sentence, startAt, textHash, request,
                    () => receivedCue = true);
                if (fallbackReason is not null && status is not null && status != LocalSpeech.CompletedStatus)
                    status = $"{fallbackReason} {status}";
            }

            if (request != speechRequest) return;
            utteranceActive = false;
            activeSpeechSource = null;
            SourceText.SpokenWord = null;
            PauseSpeech.IsEnabled = false;
            if (status == LocalSpeech.CompletedStatus)
            {
                RecordReadingActivity();
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
            SpeechStatus.Text = status == LocalSpeech.CompletedStatus && !modelVoice && !receivedCue
                ? $"{status} Межі слів від голосу не отримано." : status;
        RefreshSpeechResume();
    }

    private Task<string?> PlaySystemSentenceAsync(ReaderSession session, Document document, Sentence sentence,
        int startAt, string textHash, int request, Action receivedCue) =>
        speech.SpeakAsync(sentence.Text[startAt..], document.Language, (start, length) => Dispatcher.Dispatch(() =>
        {
            if (request == speechRequest && utteranceActive && !speechPaused &&
                session.Document?.Id == document.Id && session.Sentence?.Id == sentence.Id &&
                session.Sentence?.Text == sentence.Text &&
                HighlightSpokenWord(sentence, startAt + start, length) is int spokenOffset)
            {
                receivedCue();
                pendingSpeechResume = (document.Id, new(sentence.Id, textHash, spokenOffset));
                if (lastSpeechSave == 0 || Stopwatch.GetElapsedTime(lastSpeechSave) >= TimeSpan.FromSeconds(2))
                    SaveSpeechResume();
            }
        }));

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
        RecordReadingActivity();
        if (speechPaused)
        {
            ResetReadingActivity();
            SaveSpeechResume();
        }
    }

    private void StopClicked(object? sender, EventArgs args)
    {
        RecordReadingActivity();
        StopSpeech();
    }

    private void StopSpeech()
    {
        speechRequest++;
        speech.Stop();
        CancelModelSpeech();
        speechPaused = false;
        utteranceActive = false;
        activeSpeechSource = null;
        SourceText.SpokenWord = null;
        PauseSpeech.IsEnabled = false;
        PauseSpeech.Text = "Пауза";
        SpeechStatus.Text = "Озвучення зупинено.";
        SaveSpeechResume();
        RefreshSpeechResume();
    }
}
