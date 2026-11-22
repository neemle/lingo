using Reader.Contracts;
using Reader.Native.Services;
using VocabularyCard = Reader.Contracts.Vocabulary;

namespace Reader.Native.Features.Vocabulary;

public partial class VocabularyPage
{
    private bool showingReviewHistory;

    private async void ReviewHistoryClicked(object? sender, EventArgs args)
    {
        if (showingReviewHistory ||
            sender is not Button { CommandParameter: VocabularyCard selected }) return;
        showingReviewHistory = true;
        ContentPage? historyPage = null;
        try
        {
            var activeAppearance = appearance;
            var reviews = await AppState.Session.VocabularyReviewHistoryAsync(selected.Id);
            if (appearance != activeAppearance) return;
            var card = AppState.Session.Snapshot.Vocabulary.FirstOrDefault(entry => entry.Id == selected.Id) ?? selected;
            var body = new VerticalStackLayout { Padding = 24, Spacing = 12 };
            body.Children.Add(new Label { Text = $"{card.Word} → {card.Translation}", FontSize = 24 });
            body.Children.Add(new Label { Text = DueText("Прямий напрям", card.Forward) });
            body.Children.Add(new Label { Text = DueText("Зворотний напрям", card.Reverse) });
            body.Children.Add(new Label
            {
                Text = reviews.Length == 0 ? "Повторень ще немає." : "Останні повторення",
                FontAttributes = FontAttributes.Bold,
            });
            var close = new Button { Text = "Закрити", HorizontalOptions = LayoutOptions.Start };
            body.Children.Add(close);
            foreach (var review in reviews)
            {
                var direction = review.Direction == "forward" ? "слово → переклад" : "переклад → слово";
                var grade = review.Grade switch
                {
                    0 => "Ще раз",
                    1 => "Складно",
                    2 => "Добре",
                    3 => "Легко",
                    _ => "Невідома оцінка",
                };
                body.Children.Add(new Label { Text = $"{LocalDate(review.ReviewedAt)} · {direction} · {grade}" });
            }
            var page = new ContentPage { Title = "Історія повторень", Content = new ScrollView { Content = body } };
            close.Clicked += async (_, _) =>
            {
                if (Navigation.ModalStack.LastOrDefault() != page || !close.IsEnabled) return;
                close.IsEnabled = false;
                await Navigation.PopModalAsync();
            };
            page.Disappearing += (_, _) => showingReviewHistory = false;
            historyPage = page;
            await Navigation.PushModalAsync(page);
        }
        catch (Exception error)
        {
            await DisplayAlertAsync("Історія повторень", error.Message, "Гаразд");
        }
        finally
        {
            if (historyPage is null || Navigation.ModalStack.LastOrDefault() != historyPage)
                showingReviewHistory = false;
        }
    }

    private static string DueText(string direction, ReviewState state)
    {
        var due = state.DueAt <= DateTimeOffset.UtcNow.ToUnixTimeSeconds() ? "можна повторювати" : LocalDate(state.DueAt);
        return $"{direction} · повторень: {state.Reviews} · наступне: {due}";
    }

    private static string LocalDate(long timestamp)
    {
        if (timestamp < 0 || timestamp > DateTimeOffset.MaxValue.ToUnixTimeSeconds()) return "дата недоступна";
        return DateTimeOffset.FromUnixTimeSeconds(timestamp).ToLocalTime().ToString("g");
    }
}
