using System.Text;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private sealed record PastedDocument(string Name, string Text, string Language);

    private async void PasteTextClicked(object? sender, EventArgs args)
    {
        string? clipboard = null;
        try { if (Clipboard.Default.HasText) clipboard = await Clipboard.Default.GetTextAsync(); }
        catch (Exception error) when (error is InvalidOperationException or UnauthorizedAccessException or NotSupportedException) { }
        await ImportPastedTextAsync(clipboard);
    }

    private async void PasteImageClicked(object? sender, EventArgs args)
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
        var title = new Entry { Placeholder = "Назва документа", MaxLength = 180 };
        var language = new Picker
        {
            Title = "Мова оригіналу",
            ItemsSource = DocumentLanguages.All.Select(item => item.Name).ToArray(),
            SelectedIndex = -1,
        };
        var text = new Editor { Text = initial, Placeholder = "Вставте текст, який хочете прочитати", MinimumHeightRequest = 240 };
        var status = new Label { TextColor = (Color)Application.Current!.Resources["Accent"] };
        var detect = new Button { Text = "Визначити мову", Style = (Style)Application.Current.Resources["OutlineButton"] };
        var import = new Button { Text = "Почати читати", Style = (Style)Application.Current.Resources["AccentButton"] };
        var cancel = new Button { Text = "Скасувати" };
        var form = new VerticalStackLayout { Spacing = 12, Padding = 24, MaximumWidthRequest = 680 };
        form.Children.Add(new Label { Text = "Додати текст", Style = (Style)Application.Current.Resources["SerifTitle"] });
        foreach (var field in new View[] { language, title, text, detect, status, import, cancel })
            form.Children.Add(field);
        var page = new ContentPage { Title = "Додати текст", Content = new ScrollView { Content = form } };
        page.Disappearing += (_, _) => { if (!closing) completion.TrySetResult(null); };
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
        detect.Clicked += async (_, _) =>
        {
            if (closing || string.IsNullOrWhiteSpace(text.Text)) return;
            detect.IsEnabled = false;
            var sample = text.Text;
            try
            {
                var code = await AppState.Session.DetectLanguageAsync(sample);
                if (closing || sample != text.Text) return;
                var suggestions = DocumentLanguages.All.Select((item, index) => (item, index));
                var suggestion = suggestions.FirstOrDefault(entry => entry.item.Code == code);
                if (code is not null && suggestion.item.Code == code)
                {
                    language.SelectedIndex = suggestion.index;
                    status.Text = $"Визначена мова · {suggestion.item.Name}";
                }
                else status.Text = "Виберіть мову вручну.";
            }
            catch (Exception error) { status.Text = error.Message; }
            finally { detect.IsEnabled = true; }
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
        return await completion.Task;
    }
}
