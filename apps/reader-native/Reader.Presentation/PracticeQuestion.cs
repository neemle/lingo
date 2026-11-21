using Reader.Contracts;

namespace Reader.Presentation;

public sealed record PracticeQuestion(string Prompt, string[] Answers, string[] Options)
{
    private static readonly StringComparer Comparer = StringComparer.OrdinalIgnoreCase;

    public static PracticeQuestion Create(Vocabulary card, IEnumerable<Vocabulary> vocabulary, bool reverse,
        bool includeOptions)
    {
        string PromptOf(Vocabulary entry) => reverse ? entry.Translation : entry.Word;
        string AnswerOf(Vocabulary entry) => reverse ? entry.Word : entry.Translation;

        var prompt = PromptOf(card);
        var pair = vocabulary.Where(entry =>
            Comparer.Equals(entry.Language, card.Language) &&
            Comparer.Equals(entry.TargetLanguage, card.TargetLanguage)).Append(card).ToArray();
        var promptKey = Normalize(prompt);
        var answers = pair.Where(entry => Comparer.Equals(Normalize(PromptOf(entry)), promptKey))
            .Select(AnswerOf).Where(answer => !string.IsNullOrWhiteSpace(answer))
            .DistinctBy(Normalize, Comparer).ToArray();
        if (!includeOptions) return new PracticeQuestion(prompt, answers, []);

        var accepted = answers.Select(Normalize).ToHashSet(Comparer);
        var distractors = pair.Select(AnswerOf).Where(answer =>
            !string.IsNullOrWhiteSpace(answer) && !accepted.Contains(Normalize(answer)))
            .DistinctBy(Normalize, Comparer).ToArray();

        string[] options = [];
        if (distractors.Length >= 2 && !string.IsNullOrWhiteSpace(AnswerOf(card)))
        {
            Random.Shared.Shuffle(distractors.AsSpan());
            options = [AnswerOf(card), .. distractors.Take(3)];
            Random.Shared.Shuffle(options.AsSpan());
        }

        return new PracticeQuestion(prompt, answers, options);
    }

    public bool Matches(string text) => Answers.Any(answer => Comparer.Equals(Normalize(answer), Normalize(text)));

    private static string Normalize(string value) =>
        string.Join(' ', value.Normalize().Replace('’', '\'').Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
}
