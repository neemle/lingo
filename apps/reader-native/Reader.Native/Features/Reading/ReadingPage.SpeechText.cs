using System.Text;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private const int MaxSpeechTextBytes = 1000;
    private bool editingSpeechText;

    private const string SkippedFootnote = "Виноски пропускаються під час озвучення.";
    private const string EmptyNarration = "У фрагменті немає тексту для озвучення.";

    private static bool HasNarrationText(string text) => text.EnumerateRunes().Any(Rune.IsLetterOrDigit);

    private static bool IsFootnote(Document document, Sentence sentence)
    {
        var offset = sentence.StartUtf16;
        foreach (var character in sentence.Text)
        {
            if (char.IsLetterOrDigit(character)) break;
            offset++;
        }
        return document.Blocks.Any(block => block.Footnote && offset >= block.StartUtf16 && offset < block.EndUtf16);
    }

    private static string EffectiveSpeechText(Document document, Sentence sentence)
    {
        if (IsFootnote(document, sentence)) return "";
        if (document.SpeechTextOverrides.TryGetValue(sentence.Id, out var saved))
            return saved;
        var text = sentence.Text;
        if (document.PdfPageStarts is { Length: > 0 } && text.Length > 0 &&
            (char.IsLower(text[0]) || text[^1] is '-' or '\u00ad' or '\u2010'))
            return AppState.Session.GetSpeechNarration(document.Id, sentence.Id) ?? text;
        return text;
    }

    private static string ReadingPronunciation(Document document, Sentence sentence, string speechText) =>
        (document.Format == "pdf" || document.OcrLines is { Length: > 0 }) &&
            (speechText.Contains('\n') || speechText.Any(char.IsNumber)) &&
            !document.SpeechTextOverrides.ContainsKey(sentence.Id)
            ? AppState.Session.GetSpeechNarration(document.Id, sentence.Id, pronunciationOnly: true) ?? speechText
            : speechText;

    private async void EditSpeechTextClicked(object? sender, EventArgs args)
    {
        if (DetachedPdfReader.IsVisible)
        {
            await EditDetachedSpeechTextAsync();
            return;
        }
        var session = AppState.Session;
        if (editingSpeechText || PdfPageNotice.IsVisible || session.Busy || session.Document is not { } document ||
            session.Sentence is not { } sentence) return;

        editingSpeechText = true;
        try
        {
            StopSpeech();
            await ShowSpeechTextEditorAsync(document.Language, sentence,
                document.SpeechTextOverrides.GetValueOrDefault(sentence.Id), async text =>
                {
                    await session.WaitUntilIdleAsync();
                    await session.SetSpeechTextAsync(document.Id, sentence.Id, text);
                    if (session.Error.Length > 0) throw new InvalidOperationException(session.Error);
                });
        }
        catch (Exception error) { await DisplayAlertAsync("Вимова речення", error.Message, "Гаразд"); }
        finally { editingSpeechText = false; }
    }

    private async void EditDetachedSpeechTextClicked(object? sender, EventArgs args) => await EditDetachedSpeechTextAsync();

    private async Task EditDetachedSpeechTextAsync()
    {
        var session = AppState.Session;
        if (editingSpeechText || session.Busy || loadingDetachedSentence ||
            detachedAddress is not { } address || detachedSentence is not { } current) return;

        editingSpeechText = true;
        try
        {
            ClearDetachedSpeech();
            await ShowSpeechTextEditorAsync(current.Language, current.Sentence, current.SpeechText, async text =>
            {
                await session.WaitUntilIdleAsync();
                if (session.Document?.Id != address.Document)
                    throw new InvalidOperationException("Документ змінився.");
                var updated = await session.UpdateDetachedPdfAnnotationAsync(
                    address.Document, address.Page, current.Sentence.Id, new { kind = "speechText", text });
                if (ReferenceEquals(detachedAddress, address) && detachedSentence?.Sentence.Id == current.Sentence.Id)
                {
                    detachedSentence = updated;
                    UpdateDetachedControls();
                }
            });
        }
        catch (Exception error) { await DisplayAlertAsync("Вимова речення", error.Message, "Гаразд"); }
        finally { editingSpeechText = false; }
    }

    private async Task ShowSpeechTextEditorAsync(string language, Sentence sentence, string? speechText,
        Func<string, Task> saveText)
    {
        var resources = Application.Current!.Resources;
        var editor = new Editor { Text = speechText ?? sentence.Text, HeightRequest = 160 };
        SemanticProperties.SetDescription(editor, "Текст для озвучення");
        var status = new Label { TextColor = (Color)resources["Accent"] };
        var controls = new HorizontalStackLayout { Spacing = 8 };
        var form = new VerticalStackLayout { Padding = 24, Spacing = 12, MaximumWidthRequest = 680 };
        form.Children.Add(new Label { Text = "Вимова речення", Style = (Style)resources["SerifTitle"] });
        var instruction = language == "uk"
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
            IsVisible = speechText is not null,
        };
        var accent = new Button
        {
            Text = "Поставити наголос",
            Style = (Style)resources["OutlineButton"],
            IsVisible = language == "uk",
        };
        DescribeButton(save, "Зберегти вимову речення");
        DescribeButton(cancel, "Закрити редактор вимови");
        DescribeButton(reset, "Відновити вимову за текстом оригіналу");
        DescribeButton(accent, "Поставити наголос на голосній біля курсора");
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
                await saveText(text);
                if (Navigation.ModalStack.LastOrDefault() == page)
                    await Navigation.PopModalAsync();
            }
            catch (Exception error) { status.Text = error.Message; }
            finally { controls.IsEnabled = true; }
        }
    }
}
