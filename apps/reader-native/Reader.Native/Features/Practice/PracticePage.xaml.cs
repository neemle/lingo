using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Practice;

public partial class PracticePage : ContentPage
{
    private PracticeProgress? progress;
    private bool renderingSettings;
    private bool grading;
    private bool visible;

    public PracticePage() => InitializeComponent();

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        visible = true;
        BindingContext = AppState.Session;
        if (grading) return;
        await AppState.Session.RefreshAsync();
        if (visible) Reload();
    }

    protected override void OnDisappearing()
    {
        visible = false;
        base.OnDisappearing();
    }

    private void Reload()
    {
        try { OpenQuestion(AppState.Session.GetPractice()); }
        catch (Exception error) { QueueStatus.Text = error.Message; }
    }

    private void OpenQuestion(PracticeProgress state, bool settingsChanged = false)
    {
        var session = AppState.Session;
        var vocabulary = session.Snapshot.Vocabulary;
        var reverse = state.Direction == "reverse";
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var due = vocabulary.Where(card => (reverse ? card.Reverse : card.Forward).DueAt <= now).ToArray();
        if (state.Question is null && due.FirstOrDefault() is { } card)
        {
            var question = PracticeQuestion.Create(card, vocabulary, reverse, state.Mode == 2);
            state = session.SetPractice(state with
            {
                Question = new SavedQuestion(card.Id, question.Prompt, question.Answers, question.Options),
            });
        }
        else if (settingsChanged) state = session.SetPractice(state);

        progress = state;
        Render(state, due.Length);
    }

    private void Render(PracticeProgress state, int due)
    {
        renderingSettings = true;
        try
        {
            var reverse = state.Direction == "reverse";
            ForwardDirection.Style = (Style)Resources[reverse ? "DirectionOption" : "SelectedDirection"];
            ReverseDirection.Style = (Style)Resources[reverse ? "SelectedDirection" : "DirectionOption"];
            SemanticProperties.SetDescription(ForwardDirection, reverse ? "Слово → переклад" : "Слово → переклад, вибрано");
            SemanticProperties.SetDescription(ReverseDirection, reverse ? "Переклад → слово, вибрано" : "Переклад → слово");
            Mode.SelectedIndex = state.Mode;
        }
        finally { renderingSettings = false; }

        var saved = state.Question;
        var session = AppState.Session;
        var noCards = session.Snapshot.Vocabulary.Length == 0;
        Prompt.Text = saved?.Prompt ?? (noCards ? "Знайдімо ваші перші слова" : "На сьогодні все");
        var context = saved is null
            ? noCards ? "Збережіть кілька слів під час читання, щоб почати практику."
                : "Усі картки цього напряму повторено. Можна змінити напрям або повернутися до читання."
            : state.Direction == "reverse" ? "Згадайте слово мовою оригіналу."
                : session.Snapshot.Vocabulary.FirstOrDefault(card => card.Id == saved.CardId)?.Context;
        QuestionContext.Text = context;
        QuestionContext.IsVisible = !string.IsNullOrWhiteSpace(context);
        EmptyAction.Text = session.Document is null ? "Відкрити бібліотеку" : "Продовжити читати";
        EmptyAction.IsVisible = saved is null;
        Answer.Text = "";
        Answer.IsVisible = saved?.Revealed == true;
        TypedAnswer.Text = "";
        Feedback.IsVisible = false;
        Choices.Children.Clear();
        QueueStatus.Text = saved is null ? "" : $"До повторення · {due}";
        Grades.IsVisible = saved?.Revealed == true;
        Reveal.IsVisible = saved is { Revealed: false } && state.Mode == 0;
        Choices.IsVisible = saved is { Revealed: false, Options.Length: > 0 } && state.Mode == 2;
        Typing.IsVisible = saved is { Revealed: false } && !Reveal.IsVisible && !Choices.IsVisible;
        if (Typing.IsVisible && state.Mode == 2)
            QueueStatus.Text += " Замало варіантів — введіть відповідь.";

        if (saved is null) return;
        if (saved.Revealed)
        {
            Answer.Text = string.Join(" · ", saved.Answers);
            if (saved.GivenAnswer is { } attempt)
            {
                var question = new PracticeQuestion(saved.Prompt, saved.Answers, saved.Options);
                var result = question.Matches(attempt)
                    ? "Відповідь збігається зі словником."
                    : "Порівняйте свою відповідь зі словником і оцініть себе.";
                Feedback.Text = $"Ваша відповідь: {attempt}\n{result}";
                Feedback.IsVisible = true;
            }
            return;
        }

        if (!Choices.IsVisible) return;
        foreach (var option in saved.Options)
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
        ShowAnswer(TypedAnswer.Text.Trim());
    }

    private void ShowAnswer(string? attempt)
    {
        if (grading || progress is not { Question: { Revealed: false } question } state) return;
        try
        {
            var saved = AppState.Session.SetPractice(state with
            {
                Question = question with { Revealed = true, GivenAnswer = attempt },
            });
            OpenQuestion(saved);
        }
        catch (Exception error) { QueueStatus.Text = error.Message; }
    }

    private async void GradeClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (grading || session.Busy || progress is not { Question.Revealed: true } state ||
            sender is not Button { CommandParameter: string value } || !byte.TryParse(value, out var grade))
            return;

        grading = true;
        Grades.IsEnabled = Direction.IsEnabled = Mode.IsEnabled = false;
        try
        {
            await session.GradePracticeAsync(state.Revision, grade);
            if (visible) Reload();
        }
        finally
        {
            grading = false;
            Grades.IsEnabled = Direction.IsEnabled = Mode.IsEnabled = true;
        }
    }

    private void PracticeSettingChanged(object? sender, EventArgs args) => ChangePracticeSettings(progress?.Direction);

    private void PracticeDirectionClicked(object? sender, EventArgs args)
    {
        if (sender is Button { CommandParameter: string direction } && direction != progress?.Direction)
            ChangePracticeSettings(direction);
    }

    private async void ReturnToReadingClicked(object? sender, EventArgs args) =>
        await Shell.Current.GoToAsync(AppState.Session.Document is null ? "//library" : "//reading");

    private void ChangePracticeSettings(string? direction)
    {
        if (renderingSettings || grading || progress is not { } state || Mode.SelectedIndex < 0 || direction is null) return;
        try
        {
            OpenQuestion(state with
            {
                Direction = direction,
                Mode = (byte)Mode.SelectedIndex,
                Question = null,
            }, settingsChanged: true);
        }
        catch (Exception error)
        {
            Reload();
            QueueStatus.Text = error.Message;
        }
    }
}
