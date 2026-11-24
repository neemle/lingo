using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private sealed record ModelPassage(string Document, int? Page, string Sentence, string Revision,
        string SourceLanguage, string Target);
    private ModelPassage? runningPassage;
    private CancellationTokenSource? passageCancellation;
    private (ModelPassage Address, ModelPassageTranslation Translation)? modelPassageResult;
    private bool openingPassageComparison;
    private ModelPassage? requestedCachedPassage;
    private int cachedPassageRequest;
    private (ModelPassage Address, string Text)? modelPassageError;

    private ModelPassage? CurrentModelPassage()
    {
        var session = AppState.Session;
        if (TargetCode is not { } target || session.Document is not { } document) return null;
        if (DetachedPdfReader.IsVisible)
            return !loadingDetachedSentence && detachedAddress is { } address &&
                address.Document == document.Id && address.Language == document.Language &&
                address.Text == DetachedPdfText.Text &&
                detachedSentence is { Footnote: false, RevisionId: { } revision } current &&
                current.Language == address.Language && revision == address.RevisionId
                ? new ModelPassage(document.Id, address.Page, current.Sentence.Id, revision, address.Language, target)
                : null;
        return !PdfPageNotice.IsVisible && session.Sentence is { } sentence && !IsFootnote(document, sentence) &&
            document.CurrentRevisionId is { } canonical
            ? new ModelPassage(document.Id, null, sentence.Id, canonical, document.Language, target)
            : null;
    }

    private async void TranslateWithModelClicked(object? sender, EventArgs args)
    {
        if (CurrentModelPassage() is not { } address) return;
        if (runningPassage is not null)
        {
            if (runningPassage == address)
            {
                CancelModelPassage();
                UpdateModelTranslation();
            }
            return;
        }
        var session = AppState.Session;
        var regenerate = CurrentModelTranslation(address) is not null;
        cachedPassageRequest++;
        modelPassageError = null;
        runningPassage = address;
        using var cancellation = new CancellationTokenSource();
        passageCancellation = cancellation;
        UpdateModelTranslation();
        try
        {
            var result = await session.TranslateWithModelAsync(address.Document, address.Page, address.Sentence,
                address.Target, regenerate, cancellation.Token, address.Revision, address.SourceLanguage);
            if (readingPageActive && !cancellation.IsCancellationRequested && CurrentModelPassage() == address)
                modelPassageResult = (address, result);
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { }
        catch (Exception error) when (readingPageActive && CurrentModelPassage() == address)
        {
            modelPassageError = (address, error.Message);
        }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
        finally
        {
            passageCancellation = null;
            runningPassage = null;
            if (requestedCachedPassage == address && CurrentModelTranslation(address) is null)
                requestedCachedPassage = null;
            if (readingPageActive) UpdateModelTranslation();
        }
    }

    private void CancelModelPassage()
    {
        if (passageCancellation is { IsCancellationRequested: false } cancellation)
            _ = cancellation.CancelAsync();
    }

    private void InvalidateModelPassageCache()
    {
        CancelModelPassage();
        cachedPassageRequest++;
        requestedCachedPassage = null;
        modelPassageResult = null;
        modelPassageError = null;
    }

    private void UpdateModelTranslation()
    {
        var address = CurrentModelPassage();
        var detached = address?.Page is not null;
        if (runningPassage is not null && runningPassage != address) CancelModelPassage();
        var running = address is not null && runningPassage == address;
        var canceling = passageCancellation?.IsCancellationRequested == true;
        var enabled = running ? !canceling : address is not null && runningPassage is null && !AppState.Session.Busy;
        RestoreCachedModelTranslation(address);
        TranslateWithModel.IsEnabled = enabled && !detached;
        DetachedTranslateWithModel.IsEnabled = enabled && detached;
        var translation = CurrentModelTranslation(address);
        TranslateWithModel.Text = DetachedTranslateWithModel.Text = running ? "Скасувати переклад" : translation is not null
            ? "Перегенерувати" : address is not null && modelPassageError?.Address == address
                ? "Повторити переклад" : "Перекласти моделлю";
        var actionHint = running ? "Скасувати переклад моделлю" : translation is not null
            ? "Перекласти поточний уривок моделлю заново" : "Перекласти поточний уривок локальною моделлю";
        DescribeButton(TranslateWithModel, actionHint);
        DescribeButton(DetachedTranslateWithModel, actionHint);

        var result = translation is null ? "" :
            $"{(translation.Cached ? "Збережений переклад" : "Переклад")} · {translation.ModelName}{Environment.NewLine}{translation.Text}";
        var failure = CurrentModelFailure(address);
        var status = failure is not null && translation is not null
            ? $"Попередній переклад збережено. {failure}" : failure;
        if (running)
            status = canceling ? "Скасовуємо переклад…" : translation is null
                ? "Модель перекладає уривок…" : "Модель перекладає уривок заново…";
        var hasOutput = result.Length > 0 || status is not null;
        ModelTranslationBusy.IsRunning = ModelTranslationBusy.IsVisible = running && !detached;
        DetachedModelTranslationBusy.IsRunning = DetachedModelTranslationBusy.IsVisible = running && detached;
        ModelTranslationViewport.IsVisible = ModelTranslationResult.IsVisible = !detached && hasOutput;
        DetachedModelTranslationViewport.IsVisible = DetachedModelTranslationResult.IsVisible = detached && hasOutput;
        var label = detached ? DetachedModelTranslationText : ModelTranslationText;
        var statusLabel = detached ? DetachedModelTranslationStatus : ModelTranslationStatus;
        var outputChanged = label.Text != result || statusLabel.Text != status;
        label.Text = result;
        statusLabel.Text = status;
        statusLabel.IsVisible = status is not null;

        var canShare = !running && !string.IsNullOrWhiteSpace(translation?.SourceText);
        var canCompare = !running && (canShare || failure is not null);
        ModelTranslationActions.IsVisible = canCompare && !detached;
        DetachedModelTranslationActions.IsVisible = canCompare && detached;
        ShareModelTranslation.IsVisible = DetachedShareModelTranslation.IsVisible = canShare;
        if (outputChanged && hasOutput)
        {
            var viewport = detached ? DetachedModelTranslationViewport : ModelTranslationViewport;
            if (viewport.ScrollY > 0)
                _ = RevealModelTranslationAsync(detached, detached ? DetachedModelTranslationResult : ModelTranslationResult);
        }
    }

    private void RestoreCachedModelTranslation(ModelPassage? address)
    {
        if (requestedCachedPassage == address) return;
        requestedCachedPassage = address;
        var request = ++cachedPassageRequest;
        if (address is not null && CurrentModelTranslation(address) is null)
            _ = ReadCachedModelTranslationAsync(address, request);
    }

    private async Task ReadCachedModelTranslationAsync(ModelPassage address, int request)
    {
        await Task.Delay(140);
        bool Current() => readingPageActive && request == cachedPassageRequest &&
            CurrentModelPassage() == address && runningPassage != address;
        if (!Current()) return;
        try
        {
            var translation = await AppState.Session.CachedModelTranslationAsync(
                address.Document, address.Page, address.Sentence, address.Target, address.Revision, address.SourceLanguage);
            if (translation is null || !Current()) return;
            modelPassageResult = (address, translation);
            UpdateModelTranslation();
        }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
    }

    private ModelPassageTranslation? CurrentModelTranslation(ModelPassage? address) =>
        modelPassageResult is { } saved && saved.Address == address ? saved.Translation : null;

    private string? CurrentModelFailure(ModelPassage? address) =>
        modelPassageError is { } failure && failure.Address == address ? failure.Text : null;

    private async void CompareModelTranslationClicked(object? sender, EventArgs args)
    {
        if (openingPassageComparison || CurrentModelPassage() is not { } address) return;
        var translation = CurrentModelTranslation(address);
        var failure = CurrentModelFailure(address);
        if (translation is null && failure is null) return;
        openingPassageComparison = true;
        try
        {
            var source = translation?.SourceText;
            if (string.IsNullOrWhiteSpace(source))
                source = await AppState.Session.PassageSourceAsync(address.Document, address.Page, address.Sentence);
            if (!readingPageActive || CurrentModelPassage() != address) return;

            var modelText = translation?.Text;
            if (failure is not null) modelText = modelText is null ? failure : $"{modelText}{Environment.NewLine}{failure}";
            await ShowModelComparisonAsync(address, source, translation?.ModelName ?? "Локальна модель",
                modelText ?? "Модель не повернула перекладу.");
        }
        catch (Exception error)
        {
            if (readingPageActive && CurrentModelPassage() == address)
                await DisplayAlertAsync("Порівняння перекладів", error.Message, "Гаразд");
        }
        finally { openingPassageComparison = false; }
    }

    private async Task ShowModelComparisonAsync(ModelPassage address, string source, string modelName, string modelText)
    {
        var content = new VerticalStackLayout { Padding = new Thickness(22, 14), Spacing = 12 };
        AddPassageComparisonSection(content, "Оригінал", source);
        var (offlineHeading, offlineText) = AddPassageComparisonSection(content, "Офлайн-переклад", "Перекладаємо уривок…");
        AddPassageComparisonSection(content, modelName, modelText);
        var close = new Button { Text = "Закрити", HorizontalOptions = LayoutOptions.End, Margin = new Thickness(12) };
        DescribeButton(close, "Закрити порівняння перекладів");
        var layout = new Grid { RowDefinitions = { new(GridLength.Auto), new(GridLength.Star) } };
        layout.Add(close);
        layout.Add(new ScrollView { Content = content }, 0, 1);
        var page = new ContentPage { Title = "Порівняння перекладів", Content = layout };
        close.Clicked += async (_, _) =>
        {
            if (Navigation.ModalStack.LastOrDefault() == page) await Navigation.PopModalAsync();
        };
        using var cancellation = new CancellationTokenSource();
        void OnClosed(object? sender, EventArgs args) => cancellation.Cancel();
        page.Disappearing += OnClosed;
        var presented = false;
        try
        {
            await Navigation.PushModalAsync(page);
            presented = true;
            var offline = await AppState.Session.TranslateOfflinePassageAsync(
                address.Document, address.Page, address.Sentence, address.Target, cancellation.Token);
            if (cancellation.IsCancellationRequested) return;
            if (offline is null)
                offlineText.Text = "Офлайн-переклад уривка недоступний. Перевірте мовні пакети в налаштуваннях.";
            else if (!string.Equals(offline.SourceText, source, StringComparison.Ordinal))
                offlineText.Text = "Уривок змінився. Відкрийте порівняння ще раз.";
            else
            {
                offlineText.Text = offline.Text;
                if (offline.PivotLanguage is { } pivot)
                    offlineHeading.Text += $" · {DocumentLanguages.PivotRoute(address.SourceLanguage, pivot, address.Target)}";
            }
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { }
        catch (Exception error) when (presented)
        {
            if (!cancellation.IsCancellationRequested) offlineText.Text = error.Message;
        }
        finally { page.Disappearing -= OnClosed; }
    }

    private static (Label Heading, Label Text) AddPassageComparisonSection(VerticalStackLayout content, string title, string text)
    {
        var heading = new Label { Text = title, FontAttributes = FontAttributes.Bold, FontSize = 15 };
        SemanticProperties.SetHeadingLevel(heading, SemanticHeadingLevel.Level2);
        var body = new Label { Text = text, FontFamily = "Georgia", FontSize = 19, LineHeight = 1.3 };
        content.Children.Add(new VerticalStackLayout { Spacing = 5, Children = { heading, body } });
        return (heading, body);
    }

    private async Task RevealModelTranslationAsync(bool detached, VisualElement element)
    {
        var viewport = detached ? DetachedModelTranslationViewport : ModelTranslationViewport;
        if (!viewport.IsVisible || !element.IsVisible) return;
        try { await viewport.ScrollToAsync(element, ScrollToPosition.Start, animated: false); }
        catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
    }

    private async void ShareModelTranslationClicked(object? sender, EventArgs args)
    {
        var address = CurrentModelPassage();
        if (sharingReaderText || address is null ||
            CurrentModelTranslation(address) is not { SourceText: { Length: > 0 } original } translation)
            return;
        await ShareTranslationPairAsync(new TranslationPair(original, address.SourceLanguage, translation.Text, address.Target));
    }
}
