using System.ComponentModel;
using System.Diagnostics;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage : ContentPage
{
    private (string Code, string Name)[] availableTargets = [];
    private (string Document, string Language)? translationSource;
    private string? preferredTargetCode;
    private bool choosingLanguage;
    private bool jumpingSentence;
    private bool openingOriginal;
    private bool narrowReader;
    private bool narrowPlayer;
    private ReaderLayout readingLayout;
    private bool TranslationVisible => readingLayout != ReaderLayout.Source;
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
    private string? TargetCode { get; set; }
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
    private SpeechResume? speechResume;
    private bool loadingSpeechPreferences;
    private bool speechPreferencesReady;
    public ReadingPage()
    {
        InitializeComponent();
        WordCard.PropertyChanged += WordPopupVisibilityChanged;
        DetachedWordCard.PropertyChanged += WordPopupVisibilityChanged;
        SourceText.PropertyChanged += InlinePdfWordChanged;
        SourceText.ViewportMoved += (_, _) => ScheduleSourceViewportSync(SourceText);
        DetachedPdfText.ViewportMoved += (_, _) => ScheduleSourceViewportSync(DetachedPdfText);
        DetachedPdfText.PropertyChanged += InlinePdfWordChanged;
        SegmentTranslation.PropertyChanged += PopupTranslationTextChanged;
        ModelTranslationText.PropertyChanged += PopupTranslationTextChanged;
        DetachedTranslation.PropertyChanged += PopupTranslationTextChanged;
        DetachedModelTranslationText.PropertyChanged += PopupTranslationTextChanged;
    }

    private int storageRevision;

    protected override void OnAppearing()
    {
        base.OnAppearing();
        var cacheChanged = ResetReaderViewCache();
        LoadReaderPreferences();
        inlinePdfFit = readerPreferences.PdfFit switch
        {
            "textWidth" => PdfFit.TextWidth,
            "wholePage" => PdfFit.WholePage,
            "pageWidth" => PdfFit.PageWidth,
            _ => readerPreferences.PdfFitWidth == false ? PdfFit.WholePage : PdfFit.PageWidth
        };
        pdfTranslationLayout = readerPreferences.PdfTranslationLayout ?? true;
        UpdateInlinePdfFitLabel();
        readingLayout = Enum.TryParse<ReaderLayout>(readerPreferences.Layout, ignoreCase: true, out var storedLayout)
            && Enum.IsDefined(storedLayout) ? storedLayout : ReaderLayout.Automatic;
        ArrangeReadingColumns();
        readingPageActive = true;
        AppState.ReaderCacheInvalidated += ReaderCacheInvalidated;
        AppState.VoiceResourcesChanged += VoiceResourcesChanged;
        autoTranslate = readerPreferences.AutoTranslate ?? false;
        lastAutoTranslation = null;
        lastPrefetchOrigin = null;
        UpdateAutoTranslateButton();
        BindingContext = AppState.Session;
        AppState.Session.PropertyChanged += SessionChanged;
        AppState.Session.BackgroundPageTranslated += BackgroundPageTranslated;
        AppState.Session.BackgroundTextTranslated += BackgroundTextTranslated;
#if MACCATALYST
        AppState.Session.BackgroundPdfTextCached += BackgroundPdfTextCached;
#endif
        SyncReaderPdf();
        SyncInlineOriginalMode();
        SyncAnalysisPrefetch();
        BeginReadingHistory();
        UpdateHighlight();
        _ = RestoreReadingWordAsync();
        readinessPair = null;
        UpdateTranslation();
        if (cacheChanged && DetachedPdfReader.IsVisible) ResetDetachedTranslation();
        loadingSpeechPreferences = true;
        try
        {
            ApplySpeechPreferences();
        }
        finally { loadingSpeechPreferences = false; }
        speechPreferencesReady = true;
        RefreshSpeechResume();
        RefreshVoices(force: true);
    }

    protected override void OnDisappearing()
    {
        readingPageActive = false;
        AppState.ReaderCacheInvalidated -= ReaderCacheInvalidated;
        AppState.VoiceResourcesChanged -= VoiceResourcesChanged;
        AppState.TranslationWorker?.SetReaderTarget(null);
        readerOcrReview.CancelInference();
        CancelModelPassage();
        CancelPopupTranslation();
        CancelWordExplanation();
        CancelSyntaxExplanation();
        RestoreCachedModelTranslation(null);
        dictionaryFocus = null;
        documentFindFocus = null;
        StopReaderPdf();
        inlineOriginalDocumentId = null;
        ClearInlineOriginal();
        StopAnalysisPrefetch();
        CancelScheduledTranslation();
        EndReadingHistory(keepVisit: false);
        StopSpeech();
        StopWordDetail();
        voiceCatalogRevision++;
        readinessRequest++;
        selection = null;
        SetSelectedSense(null);
        WordCard.IsVisible = false;
        Translation.Text = "";
        AppState.Session.BackgroundPageTranslated -= BackgroundPageTranslated;
        AppState.Session.BackgroundTextTranslated -= BackgroundTextTranslated;
#if MACCATALYST
        AppState.Session.BackgroundPdfTextCached -= BackgroundPdfTextCached;
#endif
        AppState.Session.PropertyChanged -= SessionChanged;
        base.OnDisappearing();
    }

    private bool ResetReaderViewCache()
    {
        if (storageRevision == AppState.StorageRevision) return false;
        storageRevision = AppState.StorageRevision;
        translationSource = null;
        TargetCode = null;
        preferredTargetCode = null;
        translationWindows.Clear();
        prefetchAttempts.Clear();
        CancelScheduledTranslation();
        translationCacheVersion++;
        lastCacheRestore = null;
        lastAutoTranslation = null;
        machineStatus = null;
        readinessPair = null;
        readinessRequest++;
        if (documentFindFocus?.Translation is not null) documentFindFocus = null;
        pdfLayoutRequestId++;
        pdfLayoutRequest = null;
        ReleasePdfLayoutTranslation()?.Cancel();
        pdfLayoutRows = [];
        pdfLayoutRowsStale = false;
        pdfLayoutStatus = null;
        recentPdfTranslations.Clear();
        overflowContent = null;
        detachedTranslationCacheVersion++;
        detachedMachineTranslation = null;
        InvalidateModelPassageCache();
        return true;
    }

    private void ReaderCacheInvalidated()
    {
        if (!readingPageActive || !ResetReaderViewCache()) return;
        LoadReaderPreferences();
        UpdateTranslation();
        if (DetachedPdfReader.IsVisible) ResetDetachedTranslation();
    }

    internal void PrepareWindowClose()
    {
        readingPageActive = false;
        AppState.ReaderCacheInvalidated -= ReaderCacheInvalidated;
        AppState.VoiceResourcesChanged -= VoiceResourcesChanged;
        ReleasePdfLayoutTranslation()?.Cancel();
        readerOcrReview.CancelInference();
        CancelModelPassage();
        CancelPopupTranslation();
        CancelWordExplanation();
        CancelSyntaxExplanation();
        CancelScheduledTranslation();
        StopAnalysisPrefetch();
#if MACCATALYST
        CancelReaderPdfOcr();
#endif
        if (speakingDetachedSentence) ClearDetachedSpeech();
        StopWordDetail();
        StopSpeech();
    }

    private void SessionChanged(object? sender, PropertyChangedEventArgs args)
    {
        // Notifications describe a completed session state. Update once per state change.
        if (args.PropertyName == nameof(ReaderSession.Snapshot))
        {
            SyncReaderPdf();
            SyncInlineOriginalMode();
            SyncAnalysisPrefetch();
            SyncReadingHistory();
            UpdateHighlight();
            _ = RestoreReadingWordAsync();
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
        UpdateSourceLanguageButton(document);
        var word = document is not null && sentence is not null ? ReadingWord(document, sentence) : null;
        var focus = document is not null && sentence is not null
            ? DictionaryFocus(document.Id, document.CurrentRevisionId, sentence, page: null) ??
                FindSourceFocus(document.Id, document.CurrentRevisionId, sentence, page: null) : null;
        SourceText.HighlightStart = focus?.StartUtf16 ?? word?.Start ?? sentence?.StartUtf16 ?? 0;
        SourceText.HighlightLength = focus?.LengthUtf16 ?? word?.Length ??
            (sentence is null ? 0 : sentence.EndUtf16 - sentence.StartUtf16);
        UpdateInlinePdfHighlight();
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
        if (width <= 0 || ReadingColumns is null || SourcePane is null || TranslationPane is null
            || ReaderLayoutButton is null || SourceHint is null || SourceLanguage is null
            || PlayerMain is null || PlayerDocument is null || PlayerTransport is null) return;

        var stackColumns = width < 720;
        if (narrowReader != stackColumns)
        {
            narrowReader = stackColumns;
            ArrangeReadingColumns();
        }

        var wrapPlayer = width < 600;
        if (narrowPlayer != wrapPlayer)
        {
            narrowPlayer = wrapPlayer;
            PlayerMain.RowDefinitions[1].Height = wrapPlayer ? GridLength.Auto : 0;
            PlayerMain.RowSpacing = wrapPlayer ? 5 : 0;
            Grid.SetColumnSpan(PlayerDocument, wrapPlayer ? 2 : 1);
            Grid.SetColumn(PlayerTransport, wrapPlayer ? 0 : 1);
            Grid.SetColumnSpan(PlayerTransport, wrapPlayer ? 3 : 1);
            Grid.SetRow(PlayerTransport, wrapPlayer ? 1 : 0);
            PlayerTransport.HorizontalOptions = wrapPlayer ? LayoutOptions.Center : LayoutOptions.Fill;
        }
    }

    private void TogglePlaybackOptions(object? sender, EventArgs args)
    {
        PlaybackOptions.IsVisible = !PlaybackOptions.IsVisible;
        DescribeButton(PlaybackOptionsButton,
            PlaybackOptions.IsVisible ? "Сховати параметри озвучення" : "Показати параметри озвучення");
    }

    private void PauseSpeechChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(Button.Text) && sender is Button button)
            DescribeButton(button, speechPaused ? "Продовжити озвучення" : "Призупинити озвучення");
        if (args.PropertyName is nameof(Button.IsEnabled) or nameof(Button.Text))
            UpdatePrimarySpeechButton();
    }

    private void ReaderCommandTextChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName != nameof(Button.Text) || sender is not Button { Text: { Length: > 0 } text } button)
            return;
        var hint = text switch
        {
            "Більше" => "Показати додаткові дії зі словом",
            "Згорнути" => "Сховати додаткові дії зі словом",
            "Дії OCR…" => "Відкрити дії з розпізнаним текстом сторінки",
            "Сторінка PDF" => "Показати оригінальну сторінку PDF",
            "Фото" => "Показати оригінальне фото",
            "Розпізнаний текст" => "Показати розпізнаний текст",
            "Завантажуємо PDF…" or "Завантажуємо фото…" => "Повернутися до розпізнаного тексту",
            "Закладка" => "Додати закладку",
            "Нотатка…" => "Додати нотатку",
            "Свій переклад…" => "Додати свій переклад",
            "Значення" => "Показати словникові значення",
            _ when text.StartsWith("Значення · ", StringComparison.Ordinal) => "Показати словникові значення",
            _ => text,
        };
        DescribeButton(button, hint);
    }

    private void UpdatePrimarySpeechButton()
    {
        if (PrimarySpeech is null || PauseSpeech is null) return;
        var playing = PauseSpeech.IsEnabled && !speechPaused;
        PrimarySpeech.Text = playing ? "Ⅱ" : "▶";
        var label = playing ? "Призупинити озвучення"
            : PauseSpeech.IsEnabled && speechPaused ? "Продовжити озвучення"
            : speechResume is not null && !utteranceActive && ResumeSpeech.Text is { } resumeLabel ? resumeLabel
            : "Відтворити озвучення";
        DescribeButton(PrimarySpeech, label);
    }

    private void ArrangeReadingColumns()
    {
        var revealTranslation = !TranslationPane.IsVisible && TranslationVisible;
        var sourceVisible = readingLayout != ReaderLayout.Translation;
        var split = sourceVisible && TranslationVisible;
        var stacked = split && (readingLayout == ReaderLayout.Stacked
            || readingLayout == ReaderLayout.Automatic && narrowReader);
        var columns = split && !stacked;
        SourcePane.IsVisible = sourceVisible;
        UpdateSourceLanguageButton(AppState.Session.Document);
        SourceHint.IsVisible = sourceVisible && !PdfPageNotice.IsVisible;
        TranslationPane.IsVisible = TranslationVisible;
        ReadingColumns.ColumnDefinitions[1].Width = columns ? GridLength.Star : 0;
        ReadingColumns.RowDefinitions[1].Height = stacked ? GridLength.Star : 0;
        ReadingColumns.ColumnSpacing = columns ? 1 : 0;
        ReadingColumns.RowSpacing = stacked ? 1 : 0;
        Grid.SetColumn(TranslationPane, columns ? 1 : 0);
        Grid.SetRow(TranslationPane, stacked ? 1 : 0);
        DetachedSourcePane.IsVisible = sourceVisible;
        DetachedTargetPane.IsVisible = TranslationVisible;
        DetachedReadingColumns.ColumnDefinitions[1].Width = columns ? GridLength.Star : 0;
        DetachedReadingColumns.RowDefinitions[1].Height = stacked ? GridLength.Star : 0;
        DetachedReadingColumns.ColumnSpacing = columns ? 1 : 0;
        DetachedReadingColumns.RowSpacing = stacked ? 1 : 0;
        Grid.SetColumn(DetachedTargetPane, columns ? 1 : 0);
        Grid.SetRow(DetachedTargetPane, stacked ? 1 : 0);
        AlignPdfViewports();
        var layout = Layouts.First(item => item.Mode == readingLayout);
        ReaderLayoutButton.Text = layout.Compact;
        DescribeButton(ReaderLayoutButton, $"Змінити розкладку читання. Зараз {layout.Label}");
        if (revealTranslation) RequestTranslationScroll();
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
            SaveReaderPreference(ReaderPreferenceChange.Layout(LayoutName(readingLayout)));
            ArrangeReadingColumns();
            RefreshPdfTranslationLayout();
            ScheduleAutoTranslation();
        }
        finally { ReaderLayoutButton.IsEnabled = true; }
    }

    private void DecreaseReaderFont(object? sender, EventArgs args) => ChangeReadingSize(-1);
    private void IncreaseReaderFont(object? sender, EventArgs args) => ChangeReadingSize(1);

    private void ChangeReadingSize(int change)
    {
        var size = Math.Clamp(SourceText.ReadingFontSize + change, 18, 30);
        SourceText.ReadingFontSize = DetachedPdfText.ReadingFontSize = size;
        SegmentTranslation.FontSize = size - 2;
        UpdateTranslation();
    }

    private void UpdateTranslationLanguages(Document? document)
    {
        (string Document, string Language)? address = document is null ? null : (document.Id, document.Language);
        if (address == translationSource) return;
        var sameDocument = address is { } opened && translationSource is { } previous &&
            opened.Document == previous.Document;
        translationSource = address;
        var source = document?.Language;
        availableTargets = DocumentLanguages.All.Where(language => language.Code != source).ToArray();
        var saved = source is null ? null : readerPreferences.TranslationTargets.GetValueOrDefault(source);
        TargetCode = DocumentLanguages.TranslationTarget(source, saved,
            sameDocument ? preferredTargetCode : TargetCode, sameDocument);
        if (!sameDocument) preferredTargetCode = TargetCode;
    }

    private void UpdateTranslation()
    {
        var session = AppState.Session;
        var document = session.Document;
        var source = document?.Language;
        UpdateTranslationLanguages(document);
        var targetLabel = TargetCode is { } chosenTarget ? DocumentLanguages.Name(chosenTarget) : "Мова перекладу";
        TargetLanguage.Text = targetLabel;
        DescribeButton(TargetLanguage, $"Вибрати мову перекладу. Зараз {targetLabel}");
        if (readingPageActive) AppState.TranslationWorker?.SetReaderTarget(session.Document, TargetCode);
        RefreshPdfTranslationLayout();

        var wordTranslationLabel = TargetCode is { } cardTarget
            ? $"Ваш переклад · {DocumentLanguages.Name(cardTarget)}" : "Виберіть мову перекладу";
        Translation.Placeholder = wordTranslationLabel;
        SemanticProperties.SetDescription(Translation, wordTranslationLabel);
        var footnote = session.Document is { } sourceDocument && session.Sentence is { } sourceSentence &&
            IsFootnote(sourceDocument, sourceSentence);
        var active = session.Sentence is not null && TargetCode is not null && !footnote;
        var text = TargetCode is { } target ? session.ReviewedTranslation(target) : null;
        var key = ActiveTranslationKey();
        if (source is not null && TargetCode is { } routeTarget && readinessPair != (source, routeTarget))
            RefreshReadiness((source, routeTarget));
        var window = active && document is not null && TargetCode is { } targetCode
            ? FindTranslationWindow(document, targetCode) : null;
        var unit = document is not null && window is not null ? window.UnitFor(document.Position) : null;
        var alignment = active && document is not null && TargetCode is { } machineTarget
            ? ActiveMachineAlignment(document, machineTarget) : null;
        var machine = alignment?.Translation;
        var searchHit = FoundTranslation(detached: false);
        var machineLabel = alignment?.Unit.Sources.Length > 1 ? "Машинний переклад групи речень"
            : machine?.JoinedLines == true ? "Машинний переклад об’єднаних рядків" : "Машинний переклад";
        if (alignment is { Window.Scope: var scope, Translation.PivotLanguage: var pivot })
            machineLabel = DocumentLanguages.WithPivotRoute(machineLabel, scope.Source, pivot, scope.Target);
        var currentStatus = key == statusKey ? machineStatus : null;
        if (alignment is { } result && !IsPreferredRoute(result.Window)
            && (readiness is not { Ready: true } || result.Translation.PivotLanguage is null))
            machineLabel = currentStatus is null
                ? $"Збережено раніше · {machineLabel}. {ReadinessMessage()}" : $"Збережено раніше · {machineLabel}";

        TranslationNotice.Text = footnote ? "Виноски залишаються в оригіналі."
            : !active ? "Відкрийте документ для читання."
            : searchHit is not null ? DocumentLanguages.WithPivotRoute("Збіг у збереженому перекладі",
                searchHit.SourceLanguage, searchHit.PivotLanguage, searchHit.TargetLanguage)
            : translating && currentStatus is not null && machine is null ? currentStatus
            : text is not null ? "Ваш переклад активного речення"
            : machine is not null ? (currentStatus is null ? machineLabel : $"{currentStatus} {machineLabel}")
            : currentStatus ?? ReadinessMessage();
        var newTranslation = footnote ? "" : searchHit?.Target ?? text ?? machine?.Text ?? "";
        var translationChanged = SegmentTranslation.Text != newTranslation;
        SegmentTranslation.Text = newTranslation;
        if (!translationChanged && documentFindFocus?.Translation is not null)
            HighlightActiveTranslation(detached: false, WordCard.IsVisible ? PopupGloss(detached: false) : null);
        ActiveTranslationCard.IsVisible = SegmentTranslation.Text.Length > 0;
        ShareTranslationPair.IsVisible = DisplayedTranslationPair() is not null;
        EditTranslation.Text = text is null ? "Додати свій переклад" : "Змінити переклад";
        EditTranslation.IsEnabled = active && !session.Busy && !translating;
        var canExtend = document is not null && window is not null && document.Position != window.Anchor
            && (window.MoreBefore || window.MoreAfter);
        TranslateOffline.IsEnabled = active && !session.Busy && !translating && readiness?.Ready == true
            && (window is null || !window.IsParagraph || !IsPreferredRoute(window) || canExtend
                || alignment is not null && !ReferenceEquals(unit, alignment.Value.Unit)
                || document is not null && HasUntranslatedUnits(document, window));
        TranslateSentence.IsEnabled = active && !session.Busy && !translating && readiness?.Ready == true && text is null;
        UpdateSentenceRefreshButtons();
        if (DetachedPdfReader.IsVisible) UpdateDetachedControls();
        else UpdateSentenceMenu();
        TargetLanguage.IsEnabled = document is not null && availableTargets.Length > 0 && !session.Busy;
        UpdateModelTranslation();
        UpdateSourceLocation(document, session.Sentence);
        UpdateParagraphPreview(document, window, unit);
        if (translationChanged && newTranslation.Length > 0)
        {
            RequestTranslationScroll();
            RefreshCachedPdfPageLayout();
        }
        TrackTranslationScroll(key);
        _ = RestoreCachedTranslationAsync();
        ScheduleAutoTranslation();
    }

    private string ReadinessMessage()
    {
        if (readinessError is not null) return readinessError;
        if (readiness is null) return "Перевіряємо мовні пакети…";
        if (readiness.Ready)
            return readiness.PivotLanguage is { } pivot && readinessPair is { } pair
                ? $"Доступний офлайн-переклад · {DocumentLanguages.PivotRoute(pair.Source, pivot, pair.Target)}."
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
        if (DetachedPdfReader.IsVisible) _ = RestoreDetachedTranslationAsync();
    }

    private async void OpenTranslationPackagesClicked(object? sender, EventArgs args) => await OpenTranslationPackagesAsync();

    private async Task OpenTranslationPackagesAsync()
    {
        if (readiness is not { } state) return;
        var packages = state.DirectUpgrade is { } direct ? new[] { direct } : state.Missing;
        if (packages.Length == 0) return;
        try
        {
            await Shell.Current.GoToAsync(AppShell.SettingsRoute,
                new ShellNavigationQueryParameters { ["translationPackages"] = packages });
        }
        catch (Exception error)
        {
            if (PdfUsesPageTranslation) await DisplayAlertAsync("Мовні пакети", error.Message, "Гаразд");
            else if (DetachedPdfReader.IsVisible) ShowDetachedTranslation(error.Message);
            else TranslationNotice.Text = error.Message;
        }
    }

    private TranslationKey? ActiveTranslationKey()
    {
        var session = AppState.Session;
        return session.Document is { } document && session.Sentence is { } sentence && TargetCode is { } target
            ? new TranslationKey(document.Id, sentence.Id, document.Language, target, sentence.Text)
            : null;
    }

    private void UpdateSourceLanguageButton(Document? document)
    {
        SourceLanguage.IsVisible = document is not null;
        if (document is null) return;
        var name = DocumentLanguages.Name(document.Language);
        SourceLanguage.Text = narrowReader ? document.Language.ToUpperInvariant() : name;
        DescribeButton(SourceLanguage, $"Вибрати мову оригіналу. Зараз {name}");
    }

    private async void ChooseSourceLanguageClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (choosingLanguage || session.Busy || session.Document is not { } document) return;
        choosingLanguage = true;
        try
        {
            var language = await NativeChoices.ChooseAsync(this, "Мова оригіналу", document.Title,
                DocumentLanguages.All.Select(language => (language.Code, language.Name)), document.Language);
            if (language is null || language == document.Language) return;
            await session.WaitUntilIdleAsync();
            if (!readingPageActive || session.Document is not { } current ||
                current.Id != document.Id || current.Language != document.Language)
                return;

            WordCard.IsVisible = DetachedWordCard.IsVisible = false;
            StopWordDetail();
            await session.SetDocumentLanguageAsync(document.Id, language);
        }
        finally { choosingLanguage = false; }
    }

    private async void ChooseTargetClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (choosingLanguage || session.Busy || session.Document is not { } document) return;
        choosingLanguage = true;
        try
        {
            var target = await NativeChoices.ChooseAsync(this, "Мова перекладу", null,
                availableTargets.Select(language => (language.Code, language.Name)), TargetCode);
            if (target is null || session.Document is not { } current ||
                current.Id != document.Id || current.Language != document.Language) return;

            preferredTargetCode = target;
            SaveReaderPreference(ReaderPreferenceChange.TranslationTarget(document.Language, target));
            if (target == TargetCode) return;
            TargetCode = target;
            CancelPopupTranslation();
            CancelWordExplanation();
            SetSelectedSense(null);
            WordMeanings.Children.Clear();
            WordMeanings.IsVisible = false;
            OpenMeanings.Text = $"Значення · {meanings.Length}";
            ResetDetachedMeanings();
            ResetExplanation(WordExplanationText, WordExplanationGloss);
            ResetExplanation(DetachedWordExplanationText, DetachedWordExplanationGloss);
            PositionWordPopup();
            Translation.Text = "";
            if (WordCard.IsVisible || DetachedWordCard.IsVisible)
                _ = LoadPopupTranslationAsync(DetachedWordCard.IsVisible);
            UpdateTranslation();
            ResetDetachedTranslation();
        }
        finally { choosingLanguage = false; }
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
        {
            ClearTranslationFocus();
            await session.SetReviewedTranslationAsync(document.Id, sentence.Id, target, text);
            if (session.Error.Length != 0) return;
            if (existing is not null && string.IsNullOrWhiteSpace(text))
                AppState.TranslationWorker?.TranslationReviewRemoved(document.Id, target);
            if (CurrentPdfLayoutAddress() is { } page && page.Document == document.Id && page.Target == target)
                RefreshCachedPdfPageLayout();
        }
    }

    private async void SourceSelected(object? sender, TextSelection selected) => await ShowWordAsync(selected);

    private async Task ShowWordAsync(TextSelection selected, PdfWordAnchor? pdfAnchor = null)
    {
        var session = AppState.Session;
        if (session.Document is not { } doc ||
            Reader.Presentation.SourceRanges.TrimFootnoteReference(selected, doc.Blocks) is not { } lexical)
            return;
        selected = lexical;
        dictionaryFocus = null;
        RecordReadingActivity();
        if (selection != selected)
        {
            Translation.Text = "";
            SetSelectedSense(null);
        }
        ResetExplanation(WordExplanationText, WordExplanationGloss);
        selection = selected;
        wordTranslation = null;
        vocabularyReadings = [];
        VocabularyReading.IsVisible = false;
        SelectedWord.Text = selected.ReadingText;
        ShowMeanings(null);
        ShowLocalSyntax(null, loading: true);
        OpenSpelling.IsVisible = !selected.Text.Any(char.IsWhiteSpace);
        ShowExpressionOptions(null, selected);
        KnowledgeAnalysis.Text = "Пошук у мовному пакеті…";
        ShowWordPopup(SourceText, selected, WordCard, pdfAnchor);
        var index = Array.FindIndex(doc.Sentences, sentence => selected.StartUtf16 >= sentence.StartUtf16
            && selected.StartUtf16 < sentence.EndUtf16);
        try
        {
            ReadingCursor? anchored = null;
            if (index >= 0 && index != doc.Position) StopSpeech();
            if (doc.CurrentRevisionId is { } revision)
            {
                var cursor = await session.AnchorReadingSelectionAsync(doc.Id, revision, selected);
                anchored = cursor;
                if (selection != selected || session.Document is not { } active || active.Id != doc.Id
                    || active.CurrentRevisionId != revision) return;
                if (cursor is not null)
                {
                    canonicalWord = (doc.Id, cursor);
                    UpdateHighlight();
                }
            }
            else if (index >= 0 && index != doc.Position)
                await session.SelectSentenceAsync(index);

            var context = index < 0 ? null : new WordContext(
                Reader.Presentation.SourceRanges.MaskFootnoteReferences(
                    doc.Sentences[index].Text, doc.Sentences[index].StartUtf16, doc.Blocks),
                selected.StartUtf16 - doc.Sentences[index].StartUtf16);
            if (selected.RejoinedWord is { } word && context is { } prior &&
                prior.StartUtf16 >= 0 && prior.StartUtf16 <= prior.Sentence.Length - selected.LengthUtf16 &&
                prior.Sentence.AsSpan(prior.StartUtf16, selected.LengthUtf16).SequenceEqual(selected.Text))
                context = prior with
                {
                    Sentence = prior.Sentence[..prior.StartUtf16] + word +
                        prior.Sentence[(prior.StartUtf16 + selected.LengthUtf16)..],
                };
            var lookup = selected.RejoinedWord is null &&
                anchored is { SentenceStartUtf16: int offset, TokenLengthUtf16: int length }
                && doc.Sentences.ElementAtOrDefault(anchored.Position) is { } canonical
                && canonical.StartUtf16 + offset == selected.StartUtf16 && length == selected.LengthUtf16
                ? await session.LookupSentenceTokenAsync(doc.Id, canonical.Id, anchored.RevisionId, offset)
                : await session.LookupWordAsync(doc.Language, selected.ReadingText, context);
            if (selection != selected || session.Document is not { } current || current.Id != doc.Id
                || current.Language != doc.Language || current.CurrentRevisionId != doc.CurrentRevisionId
                || !WordCard.IsVisible) return;
            ShowMeanings(lookup);
            ShowLocalSyntax(lookup);
            ShowExpressionOptions(lookup, selected);
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
                    "frenchSubjectVerbAgreement" => " · форма дієслова узгоджується з підметом",
                    "ukrainianGenitivePreposition" => " · прийменник вимагає родового відмінка",
                    "ukrainianInfinitiveComplement" => " · інфінітив після дієслова «хотіти»",
                    "englishDeterminerNoun" => " · іменникове читання після артикля",
                    _ => "",
                }) +
                $" · {(double)analysis.Observations / lookup.TotalObservations:P0}" +
                (analysis.ContextObservations == 0 ? "" : $" · збігів із сусідніми словами: {analysis.ContextObservations}"));
            KnowledgeAnalysis.Text = lookup.Analyses.Length == 0
                ? lookup.SyntaxHint is null ? $"Словоформи немає в корпусі {source}." : ""
                : $"{string.Join(Environment.NewLine, readings)}{Environment.NewLine}Частота в корпусі · {source}";
            if (lookup.Expressions.Length > 0)
                KnowledgeAnalysis.Text += Environment.NewLine + string.Join(Environment.NewLine, lookup.Expressions.Select(expression =>
                    expression.Kind == "dictionary" ? $"Словниковий вираз · {expression.Text}"
                        : $"{(expression.Kind == "particle" ? "Дієслово з часткою" : "Сталий вираз")} · {expression.Text} · у корпусі: {expression.Observations}"));
            var version = $"{lookup.Package.Version} · {lookup.Package.Revision}";
            var lemmas = lookup.Analyses.DistinctBy(analysis => (analysis.Lemma, analysis.PartOfSpeech))
                .Select(analysis => new VocabularyReadingOption($"Лема · {analysis.Lemma} · {analysis.PartOfSpeech}",
                    Lemma: new LemmaSnapshot(analysis.Lemma, selected.ReadingText, analysis.PartOfSpeech,
                        analysis.Features, lookup.Package.Name, version)));
            var expressions = lookup.Expressions.DistinctBy(expression => (expression.Canonical, expression.Kind))
                .Select(expression => new VocabularyReadingOption($"Вираз · {expression.Text}",
                    Expression: ExpressionCard(expression, lookup.Package)));
            vocabularyReadings = lemmas.Concat(expressions)
                .ToArray();
            if (vocabularyReadings.Length > 0)
            {
                VocabularyReading.ItemsSource = new[] { $"Виділена форма · {selected.ReadingText}" }
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
        if (selection is not { } selected || TargetCode is not { } target) return;
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
        var session = AppState.Session;
        var text = Translation.Text ?? "";
        var reconstructed = selectedSense is null && CurrentPopupTranslation(detached: false) is { } draft &&
            draft.Text == text ? draft.Source : null;
        await session.SaveWordAsync(selected, text, target, lemma, reading?.Expression, selectedSense, reconstructed);
        if (selection == selected && TargetCode == target && AppState.Session.Error.Length == 0)
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

    private async void ViewOriginalClicked(object? sender, EventArgs args) => await ShowOriginalAsync();

    private async void PreviewOriginalFileClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (openingOriginal || !readingPageActive || !session.CanPreviewOriginalFile || session.Document is not { } document)
            return;
        openingOriginal = true;
        try
        {
            var original = await session.OriginalAsync(document.Id);
            if (!readingPageActive || session.Document?.Id != document.Id) return;
            var bytes = original.Bytes.Select(value => checked((byte)value)).ToArray();
            await NativeFiles.PreviewBytesAsync(this, original.Name, bytes);
        }
        catch (Exception error) { await DisplayAlertAsync("Оригінал", error.Message, "Гаразд"); }
        finally { openingOriginal = false; }
    }

    internal async Task ShowOriginalAsync()
    {
        var session = AppState.Session;
        if (openingOriginal || !readingPageActive || !session.CanViewOriginal || session.Document is not { } document) return;
        openingOriginal = true;
        try
        {
            var isPdf = document.Format == "pdf";
            string name;
            int total;
            Func<int, Task<PhotoPreview>> preview;
            Func<int, CancellationToken, Func<Task>, Task>? recognizeWindow = null;
            Func<int, TextSelection, Task>? showDetachedWord = null;
            int? savedPage = null;
#if MACCATALYST
            var originalFile = isPdf ? await session.OriginalFileAsync(document.Id) : null;
            if (originalFile is not null)
            {
                if (session.Document?.Id != document.Id) return;
                name = originalFile.Name;
                total = await PdfScan.PageCountAsync(originalFile.Path);
                preview = page => PdfScan.PreviewAsync(originalFile.Path, page);
                if (document.OcrPdfPages is not null)
                {
                    recognizeWindow = (page, token, onCached) =>
                        PdfPageOcr.RecognizeWindowAsync(session, document, originalFile.Path, page, token, onCached);
                    showDetachedWord = (page, selected) => OpenDetachedOcrWordCardAsync(document.Id, page, selected);
                }
            }
            else
#endif
            {
                var original = await session.OriginalAsync(document.Id);
                if (session.Document?.Id != document.Id) return;
                var bytes = original.Bytes.Select(value => checked((byte)value)).ToArray();
                name = original.Name;
                total = isPdf ? await PdfScan.PageCountAsync(bytes) : 1;
                preview = page => isPdf ? PdfScan.PreviewAsync(bytes, page) : PhotoOcr.PreviewAsync(bytes);
            }
#if MACCATALYST
            if (isPdf && selection is null && document.PdfPageStarts is not null)
                savedPage = await session.PdfReadingPageAsync(document.Id);
#endif
            if (session.Document is not { } current || current.Id != document.Id || !readingPageActive) return;
            var initial = Math.Clamp(savedPage ?? session.OriginalPageIndex(), 0, total - 1);
            await Navigation.PushAsync(new OriginalPage(name, total, initial, isPdf, preview,
                document.Id, session, current.OcrLines ?? [], selection, selected =>
                {
                    if (!readingPageActive || session.Busy || session.Document is not { } active || active.Id != document.Id ||
                        selected.StartUtf16 < 0 || selected.LengthUtf16 <= 0 ||
                        selected.StartUtf16 > active.Text.Length - selected.LengthUtf16 ||
                        !active.Text.AsSpan(selected.StartUtf16, selected.LengthUtf16).SequenceEqual(selected.Text))
                        return Task.CompletedTask;
                    return ShowWordAsync(selected);
                }, readerOcrReview, recognizeWindow, showDetachedWord));
        }
        catch (Exception error) { await DisplayAlertAsync("Оригінал", error.Message, "Гаразд"); }
        finally { openingOriginal = false; }
    }

    private async void PreviousClicked(object? sender, EventArgs args) => await MoveActiveSentenceAsync(-1);
    private async void NextClicked(object? sender, EventArgs args) => await MoveActiveSentenceAsync(1);

    private async Task MoveActiveSentenceAsync(int direction)
    {
        documentFindFocus = null;
        if (DetachedPdfReader.IsVisible)
        {
            await MoveDetachedSentenceAsync(direction);
            return;
        }
        if (PdfPageNotice.IsVisible) return;
        if (AtPdfSentenceBoundary(direction))
        {
            await MoveToAdjacentPdfSentenceAsync(direction);
            return;
        }
        CancelDetachedSpeechTravel();
        StopSpeech();
        var session = AppState.Session;
        if (session.Document is not { } document) return;
        var position = document.Position + direction;
        var next = document.Sentences.ElementAtOrDefault(position);
        await session.SelectSentenceAsync(position, pdfPage: ActivePdfPageForSentence(document.Id, next));
    }

    private void UpdateSentenceMenu()
    {
        var session = AppState.Session;
        var enabled = !session.Busy && (DetachedPdfReader.IsVisible
            ? !loadingDetachedSentence && detachedSentence is not null
            : !PdfPageNotice.IsVisible && session.Sentence is not null);
        EditSpeechTextMenu.IsEnabled = SentenceAnalysisMenu.IsEnabled = enabled;
        UpdateOcrPageReviewMenu();
    }
    private async void JumpSentenceClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (jumpingSentence || PdfPageNotice.IsVisible || session.Busy ||
            session.Document is not { Sentences.Length: > 0 } document) return;

        jumpingSentence = true;
        try
        {
            var input = await DisplayPromptAsync("Перейти до речення", $"Введіть номер від 1 до {document.Sentences.Length}.",
                accept: "Перейти", cancel: "Скасувати", keyboard: Keyboard.Numeric, maxLength: 10,
                initialValue: (document.Position + 1).ToString());
            if (input is null || session.Busy || session.Document is not { } current ||
                current.Id != document.Id || current.CurrentRevisionId != document.CurrentRevisionId) return;
            if (!int.TryParse(input, out var number) || number < 1 || number > current.Sentences.Length)
            {
                await DisplayAlertAsync("Перейти до речення", $"Введіть номер від 1 до {current.Sentences.Length}.", "Гаразд");
                return;
            }
            if (number - 1 == current.Position) return;

            CancelDetachedSpeechTravel();
            StopSpeech();
            await session.SelectSentenceAsync(number - 1);
        }
        finally { jumpingSentence = false; }
    }

    private void SpeechPreferencesChanged(object? sender, EventArgs args)
    {
        if (!speechPreferencesReady || loadingSpeechPreferences) return;
        loadingSpeechPreferences = true;
        try
        {
            if (sender is Picker picker && picker.SelectedIndex >= 0 && picker.SelectedIndex < speechRates.Length)
            {
                var index = picker.SelectedIndex;
                Rate.SelectedIndex = DetachedRate.SelectedIndex = index;
                SaveReaderPreference(ReaderPreferenceChange.SpeechRate(speechRates[index]));
            }
            else if (sender is CheckBox checkbox)
            {
                var continuous = checkbox.IsChecked;
                ContinueSpeech.IsChecked = DetachedContinueSpeech.IsChecked = continuous;
                SaveReaderPreference(ReaderPreferenceChange.ContinuousSpeech(continuous));
                if (!continuous) CancelDetachedSpeechTravel();
            }
        }
        finally { loadingSpeechPreferences = false; }
        if (!utteranceActive) RefreshSpeechResume();
        if (detachedAddress is not null) UpdateDetachedControls();
    }

    private int SpeechRatePercent => Rate.SelectedIndex >= 0 && Rate.SelectedIndex < speechRates.Length
        ? speechRates[Rate.SelectedIndex] : DefaultSpeechRate;

    private void RefreshSpeechResume()
    {
        speechResume = null;
        ResumeSpeech.IsVisible = false;
        var session = AppState.Session;

        try
        {
            if (session.Busy || session.Document is not { } document || session.Sentence is not { } sentence) return;
            var resume = session.GetSpeechResume(document.Id);
            var speechText = EffectiveSpeechText(document, sentence);
            if (resume is null || !SpeechProgress.Matches(sentence, resume, speechText)) return;
            var voice = SelectedVoice();
            var position = voice?.Engine is null
                ? SpeechProgress.ResumeOffset(sentence, resume, speechText)
                : ModelResumePosition(resume, voice, document.Language, SpeechRatePercent);
            if (!position.HasValue) return;
            speechResume = resume;
            ResumeSpeech.Text = resume.Model is null ? "Продовжити зі збереженого слова" : "Продовжити зі збереженої позиції";
            ResumeSpeech.IsVisible = true;
        }
        catch (Exception error) { SpeechStatus.Text = $"Не вдалося прочитати позицію озвучення. {error.Message}"; }
        finally { UpdatePrimarySpeechButton(); }
    }

    internal void CheckpointSpeechPosition()
    {
        checkpointModelSpeech?.Invoke();
        SaveSpeechResume();
    }

    private void SaveSpeechResume()
    {
        if (pendingSpeechResume is not { } pending) return;
        var session = AppState.Session;
        if (session.Busy) return;
        if (session.Document is not { } document || document.Id != pending.DocumentId ||
            session.Sentence is not { } sentence ||
            !SpeechProgress.Matches(sentence, pending.Resume, EffectiveSpeechText(document, sentence)))
        {
            pendingSpeechResume = null;
            return;
        }
        try
        {
            var resume = pending.Resume;
            if (resume.Model is { } model)
                session.SetModelSpeechResume(pending.DocumentId, resume.SentenceId, resume.TextHash, model);
            else
                session.SetSpeechResume(pending.DocumentId, resume.SentenceId, resume.TextHash, resume.OffsetUtf16);
            pendingSpeechResume = null;
            lastSpeechSave = Stopwatch.GetTimestamp();
        }
        catch (Exception error) { SpeechStatus.Text = $"Не вдалося зберегти позицію озвучення. {error.Message}"; }
    }

    private async void RepeatSentenceClicked(object? sender, EventArgs args)
    {
        if (DetachedPdfReader.IsVisible)
        {
            if (speakingDetachedSentence) ClearDetachedSpeech();
            await SpeakDetachedAsync(resumePlayback: false, singleSentence: true);
            return;
        }
        await SpeakAsync(singleSentence: true);
    }

    private async void ToggleSpeechClicked(object? sender, EventArgs args)
    {
        if (!AppState.Session.CanInteract) return;
        if (DetachedPdfReader.IsVisible)
        {
            await SpeakDetachedAsync(resumePlayback: true);
            return;
        }
        if (utteranceActive)
        {
            if (PauseSpeech.IsEnabled) ToggleSpeechPause();
            return;
        }

        RefreshSpeechResume();
        await SpeakAsync(speechResume);
    }

    private async void ResumeSpeechClicked(object? sender, EventArgs args)
    {
        RefreshSpeechResume();
        if (speechResume is { } resume) await SpeakAsync(resume);
    }

    private async Task SpeakAsync(SpeechResume? resume = null, bool singleSentence = false)
    {
        CancelDetachedSpeechTravel();
        var session = AppState.Session;
        if (PdfPageNotice.IsVisible || session.Busy || session.Document is not { } initial || session.Sentence is not { } sentence) return;
        ClearTranslationFocus();
        StopWordDetail();
        var source = SpeechSourceFor(initial, sentence);
        if ((singleSentence || !ContinueSpeech.IsChecked) && !HasNarrationText(source.SpeechText))
        {
            StopSpeech();
            SpeechStatus.Text = IsFootnote(initial, sentence) ? SkippedFootnote : EmptyNarration;
            return;
        }
        CheckpointSpeechPosition();
        var request = ++speechRequest;
        speech.Stop();
        CancelModelSpeech();
        speechPaused = false;
        utteranceActive = false;
        activeSpeechSource = null;
        PauseSpeech.IsEnabled = false;

        await WaitForSpeechVoicesAsync();
        if (request != speechRequest || !IsSpeechSourceCurrent(session, source) || session.Busy) return;
        using var run = new CancellationTokenSource();
        modelSpeechRun = run;
        try { await SpeakRunAsync(session, initial.Id, resume, request, singleSentence, run.Token); }
        finally
        {
            if (ReferenceEquals(modelSpeechRun, run)) CancelModelSpeech();
        }
    }

    private async Task SpeakRunAsync(ReaderSession session, string documentId, SpeechResume? resume, int request,
        bool singleSentence, CancellationToken token)
    {
        var voice = SelectedVoice();
        var modelVoice = voice?.Engine is not null;
        var ratePercent = SpeechRatePercent;
        speech.PreferredVoice = modelVoice || voice?.Id == LocalSpeech.SystemDefaultVoice ? null : voice?.Id;
        speech.Rate = ratePercent / 100f;
        var receivedCue = false;
        var systemUsesSpeechOverride = false;
        var lastUsedSystemVoice = false;
        string? status = LocalSpeech.CompletedStatus;
        (int Page, string CompletedSentenceId)? pdfContinuation = null;

        while (request == speechRequest && session.Document is { } doc && doc.Id == documentId &&
            session.Sentence is { } sentence)
        {
            var speechText = EffectiveSpeechText(doc, sentence);
            var speakable = HasNarrationText(speechText);
            var hasSpeechOverride = speechText != sentence.Text;
            var startAt = modelVoice ? 0 : SpeechProgress.ResumeOffset(sentence, resume, speechText) ?? 0;
            var startAtMs = modelVoice && resume is not null && SpeechProgress.Matches(sentence, resume, speechText)
                ? ModelResumePosition(resume, voice, doc.Language, ratePercent) ?? 0 : 0;
            resume = null;
            var textHash = SpeechProgress.TextHash(sentence, speechText);
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
            status = LocalSpeech.CompletedStatus;
            if (speakable && modelVoice)
            {
                lastUsedSystemVoice = false;
                status = await PlayModelAsync(session, doc, sentence, voice!, ratePercent, startAtMs, textHash, request,
                    singleSentence, token);
                if (request != speechRequest || token.IsCancellationRequested) return;
            }
            var fallbackReason = status is not null && status != LocalSpeech.CompletedStatus ? status : null;
            if (speakable && (!modelVoice || fallbackReason is not null))
            {
                lastUsedSystemVoice = true;
                receivedCue = false;
                if (fallbackReason is not null)
                {
                    PauseSpeech.IsEnabled = true;
                    SpeechStatus.Text = "Це речення озвучує системний голос.";
                }
                systemUsesSpeechOverride = hasSpeechOverride;
                var pronounced = await LocalSpeech.PrepareSystemSpeechAsync(session, doc.Language, ReadingPronunciation(doc, sentence, speechText));
                if (request != speechRequest || activeSpeechSource is not { } active ||
                    !IsSpeechSourceCurrent(session, active)) return;
                status = await PlaySystemSentenceAsync(session, doc, sentence, pronounced, startAt,
                    SpeechProgress.SupportsWordResume(sentence.Text, speechText) ? textHash : null,
                    request, () => receivedCue = true);
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
                try { session.ClearSpeechResume(doc.Id, null, sentence.Id, textHash, doc.Language); }
                catch (Exception error)
                {
                    status = $"Не вдалося очистити позицію озвучення. {error.Message}";
                }
            }
            else SaveSpeechResume();
            if (status != LocalSpeech.CompletedStatus || singleSentence || !ContinueSpeech.IsChecked) break;

            int? boundary;
            try { boundary = await PdfNarrationBoundaryAsync(documentId, sentence, token); }
            catch (OperationCanceledException) when (token.IsCancellationRequested) { return; }
            if (request != speechRequest) return;
            if (!ContinueSpeech.IsChecked) break;
            if (boundary is int page)
            {
                pdfContinuation = (page, sentence.Id);
                break;
            }

            if (session.Busy || session.Document is not { } current || current.Id != documentId ||
                session.Sentence?.Id != sentence.Id || current.Position + 1 >= current.Sentences.Length)
                break;
            var next = current.Position + 1;
            await session.SelectSentenceAsync(next,
                pdfPage: ActivePdfPageForSentence(current.Id, current.Sentences[next]));
            if (request != speechRequest) return;
            if (session.Error.Length > 0 || session.Document?.Position != next)
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
        {
            SpeechStatus.Text = status;
            if (status == LocalSpeech.CompletedStatus && lastUsedSystemVoice)
            {
                if (systemUsesSpeechOverride) SpeechStatus.Text += " Застосовано текст вимови.";
                else if (!receivedCue) SpeechStatus.Text += " Межі слів від голосу не отримано.";
            }
        }
        RefreshSpeechResume();
        if (pdfContinuation is { } continuation && readingPageActive)
            _ = ContinuePdfNarrationAsync(documentId, continuation.Page, continuation.CompletedSentenceId);
    }

    private Task<string?> PlaySystemSentenceAsync(ReaderSession session, Document document, Sentence sentence,
        string pronounced, int startAt, string? resumeHash, int request, Action receivedCue)
    {
        var input = SourceRanges.ProjectPronunciation(sentence.Text, pronounced, startAt, (start, length) =>
            Dispatcher.Dispatch(() =>
            {
                if (request == speechRequest && utteranceActive && !speechPaused &&
                    session.Document?.Id == document.Id && session.Sentence?.Id == sentence.Id &&
                    session.Sentence?.Text == sentence.Text &&
                    HighlightSpokenWord(sentence, start, length) is int spokenOffset)
                {
                    receivedCue();
                    if (resumeHash is null) return;
                    pendingSpeechResume = (document.Id, new(sentence.Id, resumeHash, spokenOffset));
                    if (lastSpeechSave == 0 || Stopwatch.GetElapsedTime(lastSpeechSave) >= TimeSpan.FromSeconds(2))
                        SaveSpeechResume();
                }
            }));
        return speech.SpeakAsync(input.Text, document.Language, input.Boundary);
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

    private void PauseClicked(object? sender, EventArgs args) => ToggleSpeechPause();

    private void ToggleSpeechPause()
    {
        if (speechPaused ? !speech.Resume() : !speech.Pause()) return;
        speechPaused = !speechPaused;
        PauseSpeech.Text = speechPaused ? "Продовжити" : "Пауза";
        SpeechStatus.Text = speechPaused ? "Озвучення призупинено." : "Озвучення поточного речення…";
        RecordReadingActivity();
        if (speechPaused)
        {
            ResetReadingActivity();
            CheckpointSpeechPosition();
        }
    }

    private void StopClicked(object? sender, EventArgs args)
    {
        StopWordDetail();
        CancelDetachedSpeechTravel();
        if (speakingDetachedSentence) ClearDetachedSpeech();
        RecordReadingActivity();
        StopSpeech();
    }

    internal void StopSpeech()
    {
        CheckpointSpeechPosition();
        checkpointModelSpeech = null;
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
        if (readingPageActive) RefreshSpeechResume();
    }
}
