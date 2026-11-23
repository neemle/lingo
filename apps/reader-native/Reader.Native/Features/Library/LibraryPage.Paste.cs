using System.Text;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private sealed record PastedDocument(string Name, string Text, string Language);

    private async Task PasteTextAsync()
    {
        string? clipboard = null;
        try { if (Clipboard.Default.HasText) clipboard = await Clipboard.Default.GetTextAsync(); }
        catch (Exception error) when (error is InvalidOperationException or UnauthorizedAccessException or NotSupportedException) { }
        await ImportPastedTextAsync(clipboard);
    }

    private async Task PasteImageAsync()
    {
        if (importing || changingLanguage || AppState.Session.Busy || !DesktopImport.Gate.Wait(0)) return;
        try
        {
            if (await DesktopFiles.StageClipboardImageAsync() is { } item)
                await ImportExternalAsync(item);
            else
                ImportStatus.Text = "У буфері обміну немає зображення.";
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт зображення", error.Message, "Гаразд"); }
        finally { DesktopImport.Gate.Release(); }
    }

    private async Task ImportPastedTextAsync(string? initial)
    {
        if (importing || changingLanguage || AppState.Session.Busy || !DesktopImport.Gate.Wait(0)) return;
        importing = true;
        try
        {
            if (await ImportPastedTextCoreAsync(initial) is { } id) await AppState.OpenAsync(id);
        }
        catch (Exception error) { await DisplayAlertAsync("Імпорт тексту", error.Message, "Гаразд"); }
        finally
        {
            importing = false;
            DesktopImport.Gate.Release();
            await RefreshShareInboxAsync();
        }
    }

    private async Task<string?> ImportPastedTextCoreAsync(string? initial, ShareInboxItem? source = null)
    {
        if (initial is not null && Encoding.UTF8.GetByteCount(initial) > Reader.CoreClient.CoreClient.MaxImportBytes)
            throw new InvalidDataException("Текст перевищує 20 МБ.");
        var pasted = await CollectPastedTextAsync(initial);
        if (pasted is null) return null;
        var session = AppState.Session;
        var intake = await session.StageTextIntakeAsync(source?.Id ?? Guid.NewGuid(), pasted.Name, pasted.Text);
        if (intake.DocumentId is { } completed) return completed;
        await session.WaitUntilIdleAsync();
        await session.CompleteDocumentIntakeAsync(intake.Id, pasted.Language);
        if (session.Error.Length > 0) throw new InvalidDataException(session.Error);
        return session.Document?.Id;
    }

    private async Task<PastedDocument?> CollectPastedTextAsync(string? initial)
    {
        var completion = new TaskCompletionSource<PastedDocument?>(TaskCreationOptions.RunContinuationsAsynchronously);
        var closing = false;
        var languageChosenManually = false;
        var applyingLanguageSuggestion = false;
        var detectionRevision = 0;
        var title = new Entry { Placeholder = "Назва документа", MaxLength = 180 };
        var language = new Picker
        {
            Title = "Мова оригіналу",
            ItemsSource = DocumentLanguages.All.Select(item => item.Name).ToArray(),
            SelectedIndex = -1,
        };
        var text = new Editor { Text = initial, Placeholder = "Вставте текст, який хочете прочитати", MinimumHeightRequest = 240 };
        var status = new Label { TextColor = (Color)Application.Current!.Resources["Accent"] };
        var import = new Button { Text = "Почати читати", IsEnabled = false,
            Style = (Style)Application.Current.Resources["AccentButton"] };
        var cancel = new Button { Text = "Скасувати" };
        var form = new VerticalStackLayout { Spacing = 12, Padding = 24, MaximumWidthRequest = 680 };
        form.Children.Add(new Label { Text = "Додати текст", Style = (Style)Application.Current.Resources["SerifTitle"] });
        foreach (var field in new View[] { language, status, title, text, import, cancel })
            form.Children.Add(field);
        var page = new ContentPage { Title = "Додати текст", Content = new ScrollView { Content = form } };
        page.Disappearing += (_, _) =>
        {
            if (closing) return;
            closing = true;
            completion.TrySetResult(null);
        };
        async Task CloseAsync(PastedDocument? value)
        {
            if (closing) return;
            closing = true;
            try
            {
                await page.Navigation.PopModalAsync();
                completion.TrySetResult(value);
            }
            catch (Exception error) { completion.TrySetException(error); }
        }
        cancel.Clicked += async (_, _) => await CloseAsync(null);
        void UpdateImport() =>
            import.IsEnabled = language.SelectedIndex >= 0 && !string.IsNullOrWhiteSpace(text.Text);

        void SetSuggestedLanguage(int index)
        {
            applyingLanguageSuggestion = true;
            try { language.SelectedIndex = index; }
            finally { applyingLanguageSuggestion = false; }
        }

        async Task DetectSourceLanguageAsync(bool debounce)
        {
            if (closing || languageChosenManually) return;
            var request = ++detectionRevision;
            var sample = text.Text;
            SetSuggestedLanguage(-1);
            status.Text = "";
            if (string.IsNullOrWhiteSpace(sample)) return;
            status.Text = "Визначаємо мову…";
            bool Current() => !closing && !languageChosenManually && request == detectionRevision;
            try
            {
                if (debounce) await Task.Delay(300);
                if (!Current()) return;
                var code = await AppState.Session.DetectLanguageAsync(sample);
                if (!Current()) return;
                var suggestions = DocumentLanguages.All.Select((item, index) => (item, index));
                var index = suggestions.Where(entry => entry.item.Code == code)
                    .Select(entry => entry.index).FirstOrDefault(-1);
                SetSuggestedLanguage(index);
                status.Text = index < 0 ? "Виберіть мову оригіналу." : "";
            }
            catch (Exception error) { if (Current()) status.Text = error.Message; }
        }

        language.SelectedIndexChanged += (_, _) =>
        {
            if (!applyingLanguageSuggestion && language.SelectedIndex >= 0)
            {
                languageChosenManually = true;
                status.Text = "";
            }
            UpdateImport();
        };
        text.TextChanged += (_, _) =>
        {
            UpdateImport();
            _ = DetectSourceLanguageAsync(debounce: true);
        };
        import.Clicked += async (_, _) =>
        {
            if (language.SelectedIndex < 0)
            {
                status.Text = "Виберіть мову оригіналу.";
                return;
            }
            if (string.IsNullOrWhiteSpace(text.Text))
            {
                status.Text = "Вставте текст.";
                return;
            }
            if (Encoding.UTF8.GetByteCount(text.Text) > Reader.CoreClient.CoreClient.MaxImportBytes)
            {
                status.Text = "Текст перевищує 20 МБ.";
                return;
            }
            var name = Path.GetFileName(title.Text?.Trim().Replace('\\', '/') ?? "");
            if (string.IsNullOrWhiteSpace(name)) name = "Нотатка";
            if (!name.EndsWith(".txt", StringComparison.OrdinalIgnoreCase)) name += ".txt";
            await CloseAsync(new PastedDocument(name, text.Text, DocumentLanguages.All[language.SelectedIndex].Code));
        };
        await Navigation.PushModalAsync(page);
        _ = DetectSourceLanguageAsync(debounce: false);
        return await completion.Task;
    }
}
