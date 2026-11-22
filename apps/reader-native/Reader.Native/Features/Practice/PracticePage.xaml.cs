using System.Diagnostics;
using System.Text;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;
using VocabularyCard = Reader.Contracts.Vocabulary;

namespace Reader.Native.Features.Practice;

public partial class PracticePage : ContentPage
{
    private sealed record CollectionChoice(PracticeCollection? Scope, string Title);

    private static readonly CollectionChoice AllCards = new(null, "Усі картки");
    private const int MaxAnswerBytes = 2000;
    private const long MaxElapsedMs = 86_400_000;
    private readonly Stopwatch responseTimer = new();
    private ulong? timedRevision;
    private CollectionChoice[] collections = [AllCards];
    private VocabularyCard[] vocabulary = [];
    private PracticeProgress? progress;
    private int loadVersion;
    private bool loading;
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
        await LoadAsync(refreshLibrary: true);
    }

    protected override void OnDisappearing()
    {
        visible = false;
        responseTimer.Stop();
        loadVersion++;
        loading = false;
        base.OnDisappearing();
    }

    private async Task LoadAsync(bool refreshLibrary = false, CollectionChoice? selected = null)
    {
        var request = ++loadVersion;
        var session = AppState.Session;
        loading = true;
        UpdateControls();
        QueueStatus.Text = "Завантажуємо картки…";
        try
        {
            if (refreshLibrary)
            {
                await session.RefreshAsync();
                if (request != loadVersion) return;
                var sources = await session.DictionarySourcesAsync();
                if (request != loadVersion) return;
                SetCollections(sources);
            }

            var state = session.GetPractice();
            var scope = selected is null ? state.Collection : selected.Scope;
            var collection = collections.FirstOrDefault(choice => SameCollection(choice.Scope, scope))?.Scope ?? scope;
            var cards = collection is null ? session.Snapshot.Vocabulary
                : await session.DictionaryCollectionAsync(collection.DocumentId);
            if (request != loadVersion) return;

            var changed = selected is not null && !SameCollection(state.Collection, scope);
            if (changed || state.Question is { } question && !cards.Any(card => card.Id == question.CardId))
                OpenQuestion(state with { Collection = scope, Question = null }, cards, settingsChanged: true);
            else
                OpenQuestion(state, cards);
        }
        catch (Exception error)
        {
            if (request != loadVersion) return;
            if (progress is { } saved) ShowSelectedCollection(saved.Collection);
            QueueStatus.Text = error.Message;
        }
        finally
        {
            if (request == loadVersion)
            {
                loading = false;
                UpdateControls();
            }
        }
    }

    private void SetCollections(DictionarySource[] sources)
    {
        var documents = AppState.Session.Snapshot.Documents;
        var choices = sources
            .GroupBy(source => (source.SourceHash is not null, source.SourceHash ?? source.DocumentId))
            .Select(group =>
            {
                var source = group.First();
                var local = documents.FirstOrDefault(document => document.Id == source.DocumentId ||
                    source.SourceHash is not null && document.SourceHash == source.SourceHash);
                if (local is not null) return new CollectionChoice(new(local.Id, local.SourceHash), local.Title);

                var title = string.IsNullOrWhiteSpace(source.Title) ? "Документ" : source.Title;
                return new CollectionChoice(new(source.DocumentId, source.SourceHash), $"{title} (з архіву)");
            })
            .DistinctBy(choice => choice.Scope!.DocumentId)
            .OrderBy(choice => choice.Title, StringComparer.CurrentCulture);
        collections = [AllCards, .. choices];
        var duplicateTitles = collections.Where(choice => choice.Scope is not null)
            .GroupBy(choice => choice.Title, StringComparer.CurrentCulture)
            .Where(group => group.Count() > 1 || group.Key == AllCards.Title)
            .Select(group => group.Key)
            .ToHashSet(StringComparer.CurrentCulture);
        collections = collections.Select(choice => choice.Scope is { } scope && duplicateTitles.Contains(choice.Title)
            ? choice with { Title = $"{choice.Title} · {scope.DocumentId[..Math.Min(scope.DocumentId.Length, 8)]}" }
            : choice).ToArray();
        ShowSelectedCollection(progress?.Collection);
    }

    private void ShowSelectedCollection(PracticeCollection? scope)
    {
        renderingSettings = true;
        try
        {
            var index = Array.FindIndex(collections, choice => SameCollection(choice.Scope, scope));
            if (index < 0)
            {
                collections = [.. collections, new(scope, "Збережена добірка")];
                index = collections.Length - 1;
            }
            var titles = collections.Select(choice => choice.Title).ToArray();
            if (CollectionPicker.ItemsSource is not string[] previous || !previous.SequenceEqual(titles))
                CollectionPicker.ItemsSource = titles;
            CollectionPicker.SelectedIndex = index;
        }
        finally { renderingSettings = false; }
    }

    private static bool SameCollection(PracticeCollection? first, PracticeCollection? second) =>
        first?.DocumentId == second?.DocumentId ||
        first?.SourceHash is { } hash && hash == second?.SourceHash;

    private async void CollectionChanged(object? sender, EventArgs args)
    {
        var index = CollectionPicker.SelectedIndex;
        if (!visible || loading || grading || renderingSettings || progress is null ||
            index < 0 || index >= collections.Length) return;
        var selected = collections[index];
        if (!SameCollection(progress.Collection, selected.Scope)) await LoadAsync(selected: selected);
    }

    private void UpdateControls()
    {
        var ready = visible && !loading && !grading && progress is not null;
        QuestionPanel.IsEnabled = Direction.IsEnabled = Mode.IsEnabled = ready;
        CollectionPicker.IsEnabled = ready && collections.Length > 1;
        if (ready && timedRevision.HasValue) responseTimer.Start();
        else responseTimer.Stop();
    }

    private void OpenQuestion(PracticeProgress state, VocabularyCard[] cards, bool settingsChanged = false)
    {
        var session = AppState.Session;
        var reverse = state.Direction == "reverse";
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var due = cards.Where(card => (reverse ? card.Reverse : card.Forward).DueAt <= now).ToArray();
        if (state.Question is null && due.FirstOrDefault() is { } card)
        {
            var question = PracticeQuestion.Create(card, cards, reverse, state.Mode == 2);
            state = session.SetPractice(state with
            {
                Question = new SavedQuestion(card.Id, question.Prompt, question.Answers, question.Options),
            });
        }
        else if (settingsChanged) state = session.SetPractice(state);

        progress = state;
        vocabulary = cards;
        ShowSelectedCollection(state.Collection);
        Render(state, due.Length);
        var revision = state.Question is { Revealed: false } ? state.Revision : (ulong?)null;
        if (revision != timedRevision)
        {
            responseTimer.Reset();
            timedRevision = revision;
        }
        UpdateControls();
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
        var allCards = state.Collection is null;
        var noCards = vocabulary.Length == 0;
        var emptyTitle = allCards ? "Знайдімо ваші перші слова" : "У добірці немає карток";
        var emptyHint = allCards ? "Збережіть кілька слів під час читання, щоб почати практику."
            : "Оберіть іншу добірку або додайте слова з документа.";
        Prompt.Text = saved?.Prompt ?? (noCards ? emptyTitle : "На сьогодні все");
        var card = saved is null ? null : vocabulary.FirstOrDefault(entry => entry.Id == saved.CardId);
        var context = saved is null
            ? noCards ? emptyHint : "Усі картки цього напряму повторено. Можна змінити напрям або повернутися до читання."
            : state.Direction == "reverse" ? "Згадайте слово мовою оригіналу."
                : card?.Context;
        if (state.Direction == "forward" && card?.Sense is { } sense)
        {
            var showGloss = saved?.Revealed == true ||
                !string.Equals(sense.GlossLanguage, card.TargetLanguage, StringComparison.OrdinalIgnoreCase);
            var hint = showGloss ? sense.Gloss : sense.PartOfSpeech;
            context = string.IsNullOrWhiteSpace(context) ? hint : $"{context}{Environment.NewLine}{hint}";
        }
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
        if (!visible || loading || grading || progress is not { Question: { Revealed: false } question } state) return;
        if (attempt is not null && Encoding.UTF8.GetByteCount(attempt) > MaxAnswerBytes)
        {
            QueueStatus.Text = "Відповідь перевищує 2000 байтів. Скоротіть її та спробуйте ще раз.";
            return;
        }
        var elapsed = Math.Min(responseTimer.ElapsedMilliseconds, MaxElapsedMs);
        try
        {
            var saved = AppState.Session.SetPractice(state with
            {
                Question = question with { Revealed = true, GivenAnswer = attempt, ElapsedMs = elapsed },
            });
            OpenQuestion(saved, vocabulary);
        }
        catch (Exception error) { QueueStatus.Text = error.Message; }
    }

    private async void GradeClicked(object? sender, EventArgs args)
    {
        var session = AppState.Session;
        if (loading || grading || session.Busy || progress is not { Question.Revealed: true } state ||
            sender is not Button { CommandParameter: string value } || !byte.TryParse(value, out var grade))
            return;

        grading = true;
        UpdateControls();
        try
        {
            await session.GradePracticeAsync(state.Revision, grade);
            if (visible) await LoadAsync();
        }
        finally
        {
            grading = false;
            UpdateControls();
        }
    }

    private async void PracticeSettingChanged(object? sender, EventArgs args) =>
        await ChangePracticeSettingsAsync(progress?.Direction);

    private async void PracticeDirectionClicked(object? sender, EventArgs args)
    {
        if (sender is Button { CommandParameter: string direction } && direction != progress?.Direction)
            await ChangePracticeSettingsAsync(direction);
    }

    private async void ReturnToReadingClicked(object? sender, EventArgs args) =>
        await Shell.Current.GoToAsync(AppState.Session.Document is null ? AppShell.LibraryRoute : AppShell.ReadingRoute);

    private async Task ChangePracticeSettingsAsync(string? direction)
    {
        if (renderingSettings || loading || grading || progress is not { } state || Mode.SelectedIndex < 0 || direction is null) return;
        try
        {
            OpenQuestion(state with
            {
                Direction = direction,
                Mode = (byte)Mode.SelectedIndex,
                Question = null,
            }, vocabulary, settingsChanged: true);
        }
        catch (Exception error)
        {
            await LoadAsync();
            if (visible) QueueStatus.Text = error.Message;
        }
    }
}
