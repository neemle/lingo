namespace Reader.Native.Features.Library;

public partial class LibraryPage
{
    private async Task<string?> ChooseLibraryOptionAsync(string title, string? detail,
        IEnumerable<(string Value, string Label)> options, string? selected = null)
    {
        var completion = new TaskCompletionSource<string?>(TaskCreationOptions.RunContinuationsAsynchronously);
        var resources = Application.Current!.Resources;
        var heading = new Label { Text = title, Style = (Style)resources["SerifTitle"], FontSize = 30 };
        SemanticProperties.SetHeadingLevel(heading, SemanticHeadingLevel.Level1);
        var header = new VerticalStackLayout { Spacing = 5, Children = { heading } };
        if (detail is not null)
            header.Children.Add(new Label { Text = detail, TextColor = (Color)resources["Muted"],
                MaxLines = 2, LineBreakMode = LineBreakMode.TailTruncation });

        var list = new VerticalStackLayout { Spacing = 7 };
        var scroll = new ScrollView { Content = list };
        var cancel = new Button { Text = "Скасувати", HorizontalOptions = LayoutOptions.Start };
        var layout = new Grid {
            Padding = 24,
            RowSpacing = 14,
            MaximumWidthRequest = 540,
            RowDefinitions = {
                new RowDefinition(GridLength.Auto),
                new RowDefinition(GridLength.Star),
                new RowDefinition(GridLength.Auto)
            }
        };
        layout.Add(header, 0, 0);
        layout.Add(scroll, 0, 1);
        layout.Add(cancel, 0, 2);
        var page = new ContentPage { Title = title, Content = layout };

        var closing = false;
        page.Disappearing += (_, _) => { if (!closing) completion.TrySetResult(null); };

        async Task CloseAsync(string? value)
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
        foreach (var (value, label) in options)
        {
            var button = new Button {
                Text = label,
                Style = (Style)resources[value == selected ? "AccentButton" : "OutlineButton"]
            };
            button.Clicked += async (_, _) => await CloseAsync(value);
            list.Children.Add(button);
        }

        await Navigation.PushModalAsync(page);
        return await completion.Task;
    }
}
