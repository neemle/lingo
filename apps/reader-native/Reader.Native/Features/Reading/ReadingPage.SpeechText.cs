using System.Text;
using Reader.Contracts;
using Reader.CoreClient;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private const int MaxSpeechTextBytes = 1000;
    private bool editingSpeechText;

    private static string EffectiveSpeechText(Document document, Sentence sentence) =>
        document.SpeechTextOverrides.GetValueOrDefault(sentence.Id) ?? sentence.Text;

    private static async Task<string> PrepareSystemSpeechAsync(ReaderSession session, string language, string text)
    {
        // Longer text can still be read by the platform voice without the optional pronunciation hints.
        if (language != "uk" || Encoding.UTF8.GetByteCount(text) > 64 * 1024) return text;
        try { return await session.PrepareUkrainianSpeechAsync(text); }
        catch (CoreException) { return text; }
    }

    private async void EditSpeechTextClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (editingSpeechText || session.Busy || session.Document is not { } document ||
            session.Sentence is not { } sentence) return;

        editingSpeechText = true;
        try
        {
            StopSpeech();
            await ShowSpeechTextEditorAsync(document, sentence);
        }
        catch (Exception error) { await DisplayAlertAsync("Вимова речення", error.Message, "Гаразд"); }
        finally { editingSpeechText = false; }
    }

    private async Task ShowSpeechTextEditorAsync(Document document, Sentence sentence)
    {
        var resources = Application.Current!.Resources;
        var editor = new Editor { Text = EffectiveSpeechText(document, sentence), HeightRequest = 160 };
        SemanticProperties.SetDescription(editor, "Текст для озвучення");
        var status = new Label { TextColor = (Color)resources["Accent"] };
        var controls = new HorizontalStackLayout { Spacing = 8 };
        var form = new VerticalStackLayout { Padding = 24, Spacing = 12, MaximumWidthRequest = 680 };
        form.Children.Add(new Label { Text = "Вимова речення", Style = (Style)resources["SerifTitle"] });
        var instruction = document.Language == "uk"
            ? "Редагується лише текст для озвучення. Поставте курсор після голосної та натисніть «Поставити наголос». Наприклад, «відчинені ві́кна» й «біля вікна́»."
            : "Редагується лише текст для озвучення.";
        form.Children.Add(new Label { Text = instruction });
        form.Children.Add(editor);
        form.Children.Add(status);
        form.Children.Add(controls);

        var page = new ContentPage { Title = "Вимова речення", Content = new ScrollView { Content = form } };
        var closed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        page.Disappearing += (_, _) => closed.TrySetResult();

        var save = new Button { Text = "Зберегти", Style = (Style)resources["AccentButton"] };
        var cancel = new Button { Text = "Скасувати", Style = (Style)resources["OutlineButton"] };
        var reset = new Button
        {
            Text = "Відновити оригінал",
            Style = (Style)resources["OutlineButton"],
            IsVisible = document.SpeechTextOverrides.ContainsKey(sentence.Id),
        };
        var accent = new Button
        {
            Text = "Поставити наголос",
            Style = (Style)resources["OutlineButton"],
            IsVisible = document.Language == "uk",
        };
        controls.Children.Add(save);
        controls.Children.Add(accent);
        controls.Children.Add(reset);
        controls.Children.Add(cancel);

        accent.Clicked += (_, _) =>
        {
            var text = editor.Text ?? "";
            var caret = editor.CursorPosition + editor.SelectionLength;
            if (UkrainianStressEdit.AtCaret(text, caret) is not { } moved)
            {
                status.Text = "Поставте курсор після голосної.";
                return;
            }
            editor.Text = moved.Text;
            editor.Focus();
            editor.CursorPosition = moved.Caret;
            editor.SelectionLength = 0;
            status.Text = "";
        };

        save.Clicked += async (_, _) =>
        {
            var text = editor.Text?.Trim() ?? "";
            if (text != sentence.Text && Encoding.UTF8.GetByteCount(text) > MaxSpeechTextBytes)
                status.Text = "Текст для озвучення має вміщуватися в 1000 байтів.";
            else
                await SaveAsync(text == sentence.Text ? "" : text);
        };
        reset.Clicked += async (_, _) => await SaveAsync("");
        cancel.Clicked += async (_, _) => await Navigation.PopModalAsync();

        await Navigation.PushModalAsync(page);
        await closed.Task;

        async Task SaveAsync(string text)
        {
            if (!controls.IsEnabled) return;
            controls.IsEnabled = false;
            try
            {
                var session = AppState.Session;
                await session.WaitUntilIdleAsync();
                await session.SetSpeechTextAsync(document.Id, sentence.Id, text);
                if (session.Error.Length == 0 && Navigation.ModalStack.LastOrDefault() == page)
                    await Navigation.PopModalAsync();
                else
                    status.Text = session.Error;
            }
            catch (Exception error) { status.Text = error.Message; }
            finally { controls.IsEnabled = true; }
        }
    }
}
