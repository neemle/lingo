using Reader.Native.Services;
using Reader.Presentation;
using Entry = Reader.Contracts.Vocabulary;

namespace Reader.Native.Features.Practice;

public partial class PracticePage : ContentPage
{
    private Entry? current;
    private PracticeQuestion? question;
    private bool initialized;
    private bool grading;
    private bool IsReverse => Direction.SelectedIndex == 1;

    public PracticePage()
    {
        InitializeComponent();
        initialized = true;
    }

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        BindingContext = AppState.Session;
        if (grading) return;
        await AppState.Session.RefreshAsync();
        ShowNext();
    }

    private void ShowNext()
    {
        var vocabulary = AppState.Session.Snapshot.Vocabulary;
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var due = vocabulary.Where(entry => (IsReverse ? entry.Reverse : entry.Forward).DueAt <= now).ToArray();
        current = due.FirstOrDefault();
        question = current is null ? null : PracticeQuestion.Create(current, vocabulary, IsReverse, Mode.SelectedIndex == 2);
        Prompt.Text = question?.Prompt ?? "Немає карток для повторення";
        Answer.Text = "";
        TypedAnswer.Text = "";
        Feedback.IsVisible = false;
        Choices.Children.Clear();
        Grades.IsVisible = false;
        Reveal.IsVisible = question is not null && Mode.SelectedIndex == 0;
        Choices.IsVisible = Mode.SelectedIndex == 2 && question is { Options.Length: > 0 };
        Typing.IsVisible = question is not null && !Reveal.IsVisible && !Choices.IsVisible;
        QueueStatus.Text = $"Готові до повторення: {due.Length}.";
        if (Typing.IsVisible && Mode.SelectedIndex == 2)
            QueueStatus.Text += " Замало варіантів — введіть відповідь.";

        if (!Choices.IsVisible || question is null) return;
        foreach (var option in question.Options)
        {
            var button = new Button { Text = option };
            button.Clicked += (_, _) => ShowAnswer(option);
            Choices.Children.Add(button);
        }
    }

    private void RevealClicked(object? sender, EventArgs args) => ShowAnswer(null);

    private void CheckTyped(object? sender, EventArgs args)
    {
        if (!Typing.IsVisible || string.IsNullOrWhiteSpace(TypedAnswer.Text)) return;
        ShowAnswer(TypedAnswer.Text);
    }

    private void ShowAnswer(string? attempt)
    {
        if (question is null || Grades.IsVisible) return;
        Answer.Text = string.Join(" · ", question.Answers);
        if (attempt is not null)
        {
            var result = question.Matches(attempt)
                ? "Відповідь збігається зі словником."
                : "Порівняйте свою відповідь зі словником і оцініть себе.";
            Feedback.Text = $"Ваша відповідь: {attempt}\n{result}";
            Feedback.IsVisible = true;
        }

        Reveal.IsVisible = false;
        Typing.IsVisible = false;
        Choices.IsVisible = false;
        Grades.IsVisible = true;
    }

    private async void GradeClicked(object? sender, EventArgs args)
    {
        if (current is null || grading || AppState.Session.Busy) return;
        var grade = byte.Parse((string)((Button)sender!).CommandParameter);
        grading = true;
        Grades.IsEnabled = Direction.IsEnabled = Mode.IsEnabled = false;
        try
        {
            await AppState.Session.ReviewAsync(current.Id, IsReverse ? "reverse" : "forward", grade);
            if (AppState.Session.Error.Length == 0) ShowNext();
        }
        finally
        {
            grading = false;
            Grades.IsEnabled = Direction.IsEnabled = Mode.IsEnabled = true;
        }
    }

    private void PracticeSettingChanged(object? sender, EventArgs args)
    {
        if (initialized && !grading) ShowNext();
    }
}
