using Reader.Contracts;
using Reader.Native.Services;

namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private bool showingReadingHistory;

    private async void ReadingHistoryClicked(object? sender, EventArgs args)
    {
        if (showingReadingHistory || openingCard || importing || AppState.Session.Busy) return;
        showingReadingHistory = true;
        ContentPage? historyPage = null;
        try
        {
            var session = AppState.Session;
            var records = await session.RecentReadingSessionsAsync();
            if (!ReferenceEquals(observedSession, session)) return;
            var documents = session.Snapshot.Documents.ToDictionary(document => document.Id, StringComparer.Ordinal);
            var body = new VerticalStackLayout { Padding = 24, Spacing = 12 };
            body.Children.Add(new Label
            {
                Text = "Історія читання",
                FontSize = 28,
                FontAttributes = FontAttributes.Bold,
            });
            var close = new Button { Text = "Закрити", HorizontalOptions = LayoutOptions.Start };
            body.Children.Add(close);
            var page = new ContentPage { Title = "Історія читання", Content = new ScrollView { Content = body } };
            historyPage = page;
            page.Disappearing += (_, _) => showingReadingHistory = false;
            async Task CloseAsync(Document? document = null)
            {
                if (!body.IsEnabled || Navigation.ModalStack.LastOrDefault() != page) return;
                body.IsEnabled = false;
                try
                {
                    await Navigation.PopModalAsync();
                    if (document is not null)
                    {
                        await loading;
                        await OpenCardAsync(new LibraryCard(document, 0));
                    }
                }
                catch (Exception error)
                {
                    await DisplayAlertAsync("Історія читання", error.Message, "Гаразд");
                }
                finally { body.IsEnabled = true; }
            }
            close.Clicked += async (_, _) => await CloseAsync();
            var hasDocuments = false;
            foreach (var record in records)
            {
                if (!documents.TryGetValue(record.DocumentId, out var document)) continue;
                hasDocuments = true;
                var open = new Button { Text = document.Title, HorizontalOptions = LayoutOptions.Start };
                SemanticProperties.SetDescription(open, $"Продовжити читати {document.Title}");
                open.Clicked += async (_, _) => await CloseAsync(document);
                var started = DateTimeOffset.FromUnixTimeSeconds(record.StartedAt).ToLocalTime().ToString("g");
                var active = record.ActiveDurationSeconds == 0
                    ? "Активний час не зафіксовано"
                    : $"Активний час · {record.ActiveDurationSeconds / 60} хв {record.ActiveDurationSeconds % 60} с";
                body.Children.Add(new VerticalStackLayout
                {
                    Spacing = 3,
                    Children = { open, new Label { Text = $"{started} · {active}", FontSize = 13 } },
                });
            }
            if (!hasDocuments)
                body.Children.Add(new Label { Text = "Відкрийте документ, щоб почати історію читання." });
            await Navigation.PushModalAsync(page);
        }
        catch (Exception error)
        {
            await DisplayAlertAsync("Історія читання", error.Message, "Гаразд");
        }
        finally
        {
            if (historyPage is null || Navigation.ModalStack.LastOrDefault() != historyPage)
                showingReadingHistory = false;
        }
    }
}
