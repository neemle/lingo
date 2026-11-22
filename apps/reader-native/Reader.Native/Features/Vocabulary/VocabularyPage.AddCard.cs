using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Vocabulary;

public partial class VocabularyPage
{
    private async void AddCardClicked(object? sender, EventArgs args)
    {
        if (editingCard || transferring) return;
        var session = AppState.Session;
        var languages = DocumentLanguages.All.ToArray();
        var names = languages.Select(language => language.Name).ToArray();
        var sourceIndex = Array.FindIndex(languages, language => language.Code == (session.Document?.Language ?? "en"));
        var targetIndex = Array.FindIndex(languages, language => language.Code == (sourceIndex == 0 ? "en" : "uk"));
        var source = new Picker { ItemsSource = names, SelectedIndex = sourceIndex };
        var target = new Picker { ItemsSource = names, SelectedIndex = targetIndex };
        SemanticProperties.SetDescription(source, "Мова слова");
        SemanticProperties.SetDescription(target, "Мова перекладу");

        var word = new Entry { Placeholder = "Слово або вираз", MaxLength = 500, ReturnType = ReturnType.Next };
        var translation = new Entry { Placeholder = "Переклад", MaxLength = 2000, ReturnType = ReturnType.Done };
        word.Completed += (_, _) => translation.Focus();
        var resources = Application.Current!.Resources;
        var status = new Label { TextColor = (Color)resources["Accent"] };
        var save = new Button { Text = "Зберегти", Style = (Style)resources["AccentButton"] };
        var cancel = new Button { Text = "Скасувати", Style = (Style)resources["OutlineButton"] };
        var form = new VerticalStackLayout
        {
            Padding = 24, Spacing = 10, MaximumWidthRequest = 640,
            Children =
            {
                new Label { Text = "Додати картку", Style = (Style)resources["SerifTitle"], FontSize = 30 },
                new Label { Text = "Мова слова" }, source, word,
                new Label { Text = "Мова перекладу" }, target, translation, status,
                new HorizontalStackLayout { Spacing = 8, Children = { save, cancel } },
            },
        };
        var page = new ContentPage { Title = "Додати картку", Content = new ScrollView { Content = form } };
        page.Disappearing += (_, _) => editingCard = false;
        page.Loaded += (_, _) => page.Dispatcher.Dispatch(() => word.Focus());
        cancel.Clicked += async (_, _) =>
        {
            if (form.IsEnabled && Navigation.ModalStack.LastOrDefault() == page)
                await Navigation.PopModalAsync();
        };
        save.Clicked += async (_, _) =>
        {
            if (!form.IsEnabled) return;
            if (word.Text is not { } written || string.IsNullOrWhiteSpace(written) ||
                translation.Text is not { } meaning || string.IsNullOrWhiteSpace(meaning) ||
                source.SelectedIndex < 0 || target.SelectedIndex < 0)
            {
                status.Text = "Вкажіть слово, переклад і мови.";
                return;
            }

            form.IsEnabled = false;
            try
            {
                await session.WaitUntilIdleAsync();
                await session.SaveManualVocabularyCardAsync(written, meaning,
                    languages[source.SelectedIndex].Code, languages[target.SelectedIndex].Code);
                if (session.Error.Length > 0)
                {
                    status.Text = session.Error;
                    return;
                }
                if (Navigation.ModalStack.LastOrDefault() != page) return;
                await Navigation.PopModalAsync();
                SetPicker(AllCards);
                VocabularyQuery.Text = "";
                await RefreshCollectionsAsync(appearance);
                ShowArchiveStatus("Картка збережена в словнику.");
            }
            catch (Exception error) { status.Text = error.Message; }
            finally { form.IsEnabled = true; }
        };
        editingCard = true;
        try { await Navigation.PushModalAsync(page); }
        catch (Exception error)
        {
            editingCard = false;
            ShowArchiveStatus(error.Message);
        }
    }
}
