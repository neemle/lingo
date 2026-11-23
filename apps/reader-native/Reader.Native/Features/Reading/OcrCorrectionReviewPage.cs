using Microsoft.Maui.Controls.Shapes;
using Reader.Contracts;

namespace Reader.Native.Features.Reading;

internal sealed class OcrCorrectionReviewPage : ContentPage
{
    private readonly TaskCompletionSource<OcrLineEdit[]?> result = new(TaskCreationOptions.RunContinuationsAsynchronously);

    private OcrCorrectionReviewPage(OcrLineEdit[] corrections)
    {
        Title = "Виправлення OCR";
        var title = new Label { Text = Title, FontSize = 24, FontAttributes = FontAttributes.Bold };
        SemanticProperties.SetHeadingLevel(title, SemanticHeadingLevel.Level1);
        var instructions = new Label { Text = "Позначте рядки, які хочете замінити." };
        instructions.SetDynamicResource(Label.TextColorProperty, "Muted");
        var header = new VerticalStackLayout { Spacing = 4, Children = { title, instructions } };

        var rows = new VerticalStackLayout { Spacing = 10 };
        var choices = new CheckBox[corrections.Length];
        var apply = new Button();
        apply.SetDynamicResource(StyleProperty, "AccentButton");
        var skip = new Button { Text = "Пропустити групу" };
        skip.Clicked += (_, _) => result.TrySetResult([]);
        for (var index = 0; index < corrections.Length; index++)
        {
            var edit = corrections[index];
            var choice = new CheckBox { IsChecked = true, VerticalOptions = LayoutOptions.Start };
            SemanticProperties.SetDescription(choice, $"Застосувати виправлення {index + 1}");
            SemanticProperties.SetHint(choice, edit.Replacement);
            choices[index] = choice;
            choice.CheckedChanged += (_, _) => UpdateApplyButton();

            var comparison = new VerticalStackLayout { Spacing = 8 };
            comparison.Children.Add(ComparisonText("Розпізнано", edit.Source));
            comparison.Children.Add(ComparisonText("Запропоновано", edit.Replacement));
            var row = new Grid { ColumnDefinitions = { new(GridLength.Auto), new(GridLength.Star) }, ColumnSpacing = 8 };
            row.Add(choice);
            row.Add(comparison, 1);
            var card = new Border
            {
                Content = row,
                Padding = 12,
                StrokeShape = new RoundRectangle { CornerRadius = 9 },
                Stroke = new SolidColorBrush((Color)Application.Current!.Resources["BorderTone"]),
            };
            card.SetDynamicResource(BackgroundColorProperty, "Panel");
            rows.Children.Add(card);
        }
        apply.Clicked += (_, _) => result.TrySetResult(corrections.Where((_, index) => choices[index].IsChecked).ToArray());
        UpdateApplyButton();

        var actions = new FlexLayout { Wrap = Microsoft.Maui.Layouts.FlexWrap.Wrap };
        skip.Margin = new Thickness(0, 0, 8, 0);
        actions.Children.Add(skip);
        actions.Children.Add(apply);
        var layout = new Grid
        {
            Padding = 20,
            RowSpacing = 14,
            MaximumWidthRequest = 940,
            RowDefinitions = { new(GridLength.Auto), new(GridLength.Star), new(GridLength.Auto) },
        };
        layout.Add(header);
        layout.Add(new ScrollView { Content = rows }, 0, 1);
        layout.Add(actions, 0, 2);
        Content = layout;
        Disappearing += (_, _) => result.TrySetResult(null);

        void UpdateApplyButton()
        {
            var count = choices.Count(choice => choice?.IsChecked == true);
            apply.Text = $"Застосувати вибране ({count})";
            apply.IsEnabled = count > 0;
        }
    }

    internal static async Task<OcrLineEdit[]?> ChooseAsync(INavigation navigation, OcrLineEdit[] corrections)
    {
        var page = new OcrCorrectionReviewPage(corrections);
        await navigation.PushModalAsync(page);
        var selected = await page.result.Task;
        if (navigation.ModalStack.LastOrDefault() == page) await navigation.PopModalAsync();
        return selected;
    }

    private static VerticalStackLayout ComparisonText(string caption, string text)
    {
        var label = new Label { Text = caption, FontSize = 12, FontAttributes = FontAttributes.Bold };
        label.SetDynamicResource(Label.TextColorProperty, "Muted");
        var value = new Label { Text = text, LineBreakMode = LineBreakMode.WordWrap };
        return new VerticalStackLayout { Spacing = 2, Children = { label, value } };
    }
}
