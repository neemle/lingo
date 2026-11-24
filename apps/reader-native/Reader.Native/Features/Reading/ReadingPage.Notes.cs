using System.Text;
using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private bool showingNotes;

    private async void OpenNotesClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (showingNotes || session.Document is not { } document || session.Sentence is not { } sentence) return;
        showingNotes = true;
        try
        {
            if (document.Notes.Count != 0)
            {
                const string current = "Нотатка до поточного речення";
                var choices = document.Sentences.Select((item, index) => (item, index))
                    .Where(entry => document.Notes.ContainsKey(entry.item.Id))
                    .ToDictionary(entry => BookmarkLabel(entry.index, document.Notes[entry.item.Id]), entry => entry.index);
                var selected = await DisplayActionSheetAsync("Нотатки", "Скасувати", null, [current, .. choices.Keys]);
                if (selected is null || session.Document?.Id != document.Id) return;
                if (choices.TryGetValue(selected, out var index))
                {
                    StopSpeech();
                    await session.WaitUntilIdleAsync();
                    if (session.Document?.Id != document.Id) return;
                    await session.SelectSentenceAsync(index);
                    sentence = document.Sentences[index];
                }
                else if (selected != current) return;
            }
            await ShowNoteEditorAsync(document, sentence);
        }
        catch (Exception error) { await DisplayAlertAsync("Нотатки", error.Message, "Гаразд"); }
        finally { showingNotes = false; }
    }

    private Task ShowNoteEditorAsync(Document document, Sentence sentence)
    {
        document.Notes.TryGetValue(sentence.Id, out var existing);
        return ShowNoteEditorAsync(sentence, existing, async text =>
        {
            var session = AppState.Session;
            await session.WaitUntilIdleAsync();
            await session.SetSentenceNoteAsync(document.Id, sentence.Id, text);
            if (session.Error.Length > 0) throw new InvalidOperationException(session.Error);
        });
    }

    private async Task ShowNoteEditorAsync(Sentence sentence, string? existing, Func<string, Task> saveNote)
    {
        var resources = Application.Current!.Resources;
        var editor = new Editor { Text = existing, Placeholder = "Напишіть нотатку", MinimumHeightRequest = 180, MaxLength = 4000 };
        var status = new Label { TextColor = (Color)resources["Accent"] };
        var actions = new HorizontalStackLayout { Spacing = 8 };
        var form = new VerticalStackLayout { Padding = 24, Spacing = 12, MaximumWidthRequest = 680 };
        form.Children.Add(new Label { Text = "Нотатка до речення", Style = (Style)resources["SerifTitle"] });
        form.Children.Add(new Label { Text = sentence.Text, MaxLines = 4, LineBreakMode = LineBreakMode.TailTruncation });
        form.Children.Add(editor);
        form.Children.Add(status);
        form.Children.Add(actions);
        var page = new ContentPage { Title = "Нотатка", Content = new ScrollView { Content = form } };
        var closed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        page.Disappearing += (_, _) => closed.TrySetResult();
        var save = new Button { Text = "Зберегти", Style = (Style)resources["AccentButton"] };
        var cancel = new Button { Text = "Закрити", Style = (Style)resources["OutlineButton"] };
        var remove = new Button { Text = "Видалити", IsVisible = existing is not null, Style = (Style)resources["OutlineButton"] };
        DescribeButton(save, "Зберегти нотатку до речення");
        DescribeButton(cancel, "Закрити редактор нотатки");
        DescribeButton(remove, "Видалити нотатку до речення");
        actions.Children.Add(save);
        actions.Children.Add(cancel);
        actions.Children.Add(remove);
        cancel.Clicked += async (_, _) => await Navigation.PopModalAsync();
        save.Clicked += async (_, _) =>
        {
            var text = editor.Text?.Trim() ?? "";
            if (text.Length == 0 || Encoding.UTF8.GetByteCount(text) > 4000)
                status.Text = "Введіть нотатку до 4 КБ.";
            else await SaveAsync(text);
        };
        remove.Clicked += async (_, _) =>
        {
            if (await page.DisplayAlertAsync("Видалити нотатку?", "Цю нотатку буде видалено з бібліотеки.", "Видалити", "Скасувати"))
                await SaveAsync("");
        };

        await Navigation.PushModalAsync(page);
        await closed.Task;

        async Task SaveAsync(string text)
        {
            if (!actions.IsEnabled) return;
            actions.IsEnabled = false;
            try
            {
                await saveNote(text);
                if (Navigation.ModalStack.LastOrDefault() == page)
                    await Navigation.PopModalAsync();
            }
            catch (Exception error) { status.Text = error.Message; }
            finally { actions.IsEnabled = true; }
        }
    }
}
