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
            return detachedAddress is { } address && detachedSentence is { Footnote: false, RevisionId: { } revision } current
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
                address.Target, regenerate, cancellation.Token);
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

        var result = translation is null ? null :
            $"{(translation.Cached ? "Збережений переклад Qwen" : "Переклад Qwen 2B")}{Environment.NewLine}{translation.Text}";
        if (modelPassageError is { } failure && failure.Address == address)
            result = result is null ? failure.Text : $"{result}{Environment.NewLine}{failure.Text}";
        ModelTranslationBusy.IsRunning = ModelTranslationBusy.IsVisible = running && !detached;
        DetachedModelTranslationBusy.IsRunning = DetachedModelTranslationBusy.IsVisible = running && detached;
        ModelTranslationViewport.IsVisible = ModelTranslationResult.IsVisible = !detached && (running || result is not null);
        DetachedModelTranslationViewport.IsVisible = DetachedModelTranslationResult.IsVisible = detached && (running || result is not null);
        var text = running ? canceling ? "Скасовуємо переклад…" : "Qwen перекладає абзац…" : result ?? "";
        var label = detached ? DetachedModelTranslationText : ModelTranslationText;
        var textChanged = label.Text != text;
        label.Text = text;

        var canCompare = !running && !string.IsNullOrWhiteSpace(translation?.SourceText);
        ModelTranslationActions.IsVisible = canCompare && !detached;
        DetachedModelTranslationActions.IsVisible = canCompare && detached;
        if (textChanged && (running || result is not null))
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
                address.Document, address.Page, address.Sentence, address.Target);
            if (translation is null || !Current()) return;
            modelPassageResult = (address, translation);
            UpdateModelTranslation();
        }
        catch (Exception error) { System.Diagnostics.Debug.WriteLine(error.Message); }
    }

    private ModelPassageTranslation? CurrentModelTranslation(ModelPassage? address) =>
        modelPassageResult is { } saved && saved.Address == address ? saved.Translation : null;

    private async void CompareModelTranslationClicked(object? sender, EventArgs args)
    {
        var address = CurrentModelPassage();
        if (openingPassageComparison || address is null ||
            CurrentModelTranslation(address) is not { SourceText: { Length: > 0 } source } translation)
            return;

        var content = new VerticalStackLayout { Padding = new Thickness(22, 14), Spacing = 12 };
        AddPassageComparisonSection(content, "Оригінал", source);
        var (offlineHeading, offlineText) = AddPassageComparisonSection(content, "Офлайн-переклад", "Перекладаємо уривок…");
        AddPassageComparisonSection(content, "Qwen", translation.Text);
        var close = new Button { Text = "Закрити", HorizontalOptions = LayoutOptions.End, Margin = new Thickness(12) };
        var layout = new Grid { RowDefinitions = { new(GridLength.Auto), new(GridLength.Star) } };
        layout.Add(close);
        layout.Add(new ScrollView { Content = content }, 0, 1);
        var page = new ContentPage { Title = "Порівняння перекладів", Content = layout };
        close.Clicked += async (_, _) =>
        {
            if (Navigation.ModalStack.LastOrDefault() == page) await Navigation.PopModalAsync();
        };
        openingPassageComparison = true;
        try
        {
            await Navigation.PushModalAsync(page);
            var offline = await AppState.Session.TranslateOfflinePassageAsync(
                address.Document, address.Page, address.Sentence, address.Target);
            if (offline is null)
                offlineText.Text = "Офлайн-переклад уривка недоступний. Перевірте мовні пакети в налаштуваннях.";
            else if (!string.Equals(offline.SourceText, source, StringComparison.Ordinal))
                offlineText.Text = "Уривок змінився. Оновіть переклад Qwen і порівняйте знову.";
            else
            {
                offlineText.Text = offline.Text;
                if (offline.PivotLanguage is { } pivot)
                    offlineHeading.Text += $" · {DocumentLanguages.PivotRoute(address.SourceLanguage, pivot, address.Target)}";
            }
        }
        catch (Exception error) { offlineText.Text = error.Message; }
        finally { openingPassageComparison = false; }
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
