using Reader.Native.Services;
using Entry = Reader.Contracts.Vocabulary;

namespace Reader.Native.Features.Practice;

public partial class PracticePage : ContentPage
{
    private Entry? current;
    private string ReviewDirection => Direction.SelectedIndex == 1 ? "reverse" : "forward";
    public PracticePage() => InitializeComponent();

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        BindingContext = AppState.Session;
        await AppState.Session.RefreshAsync();
        ShowNext();
    }

    private void ShowNext()
    {
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var due = AppState.Session.Snapshot.Vocabulary.Where(entry =>
            (ReviewDirection == "forward" ? entry.Forward : entry.Reverse).DueAt <= now).ToArray();
        current = due.FirstOrDefault();
        Prompt.Text = current is null ? "На сьогодні все" :
            ReviewDirection == "forward" ? current.Word : current.Translation;
        Answer.Text = "";
        QueueStatus.Text = $"Готові до повторення: {due.Length}. Розклад зберігає Rust-ядро.";
        Reveal.IsVisible = current is not null;
        Grades.IsVisible = false;
    }

    private void RevealClicked(object? sender, EventArgs args)
    {
        if (current is null) return;
        Answer.Text = ReviewDirection == "forward" ? current.Translation : current.Word;
        Reveal.IsVisible = false;
        Grades.IsVisible = true;
    }

    private async void GradeClicked(object? sender, EventArgs args)
    {
        if (current is null || AppState.Session.Busy) return;
        var grade = byte.Parse((string)((Button)sender!).CommandParameter);
        Grades.IsEnabled = false;
        await AppState.Session.ReviewAsync(current.Id, ReviewDirection, grade);
        Grades.IsEnabled = true;
        if (AppState.Session.Error.Length == 0) ShowNext();
    }

    private void DirectionChanged(object? sender, EventArgs args)
    {
        if (QueueStatus is not null) ShowNext();
    }
}
