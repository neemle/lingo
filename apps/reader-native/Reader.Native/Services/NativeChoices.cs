namespace Reader.Native.Services;

internal static class NativeChoices
{
    private sealed record Choice(string Value, string Label, Style Style);

    internal static async Task<string?> ChooseAsync(Page owner, string title, string? detail,
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

        var selectedStyle = (Style)resources["AccentButton"];
        var regularStyle = (Style)resources["OutlineButton"];
        var list = new CollectionView {
            ItemsLayout = new LinearItemsLayout(ItemsLayoutOrientation.Vertical) { ItemSpacing = 7 },
            SelectionMode = SelectionMode.None,
            ItemsSource = options.Select(option =>
                new Choice(option.Value, option.Label, option.Value == selected ? selectedStyle : regularStyle)).ToArray()
        };
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
        layout.Add(list, 0, 1);
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
        list.ItemTemplate = new DataTemplate(() =>
        {
            var button = new Button();
            button.BindingContextChanged += (_, _) =>
            {
                if (button.BindingContext is not Choice choice) return;
                button.Text = choice.Label;
                button.Style = choice.Style;
            };
            button.Clicked += async (_, _) =>
            {
                if (button.BindingContext is Choice choice) await CloseAsync(choice.Value);
            };
            return button;
        });

        await owner.Navigation.PushModalAsync(page);
        return await completion.Task;
    }
}
