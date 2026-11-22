namespace Reader.Presentation;

public sealed record SpeechVoice(string Id, string Name, string Language)
{
    public static bool MatchesLanguage(string tag, string language)
    {
        tag = tag.Replace('_', '-');
        return tag.Equals(language, StringComparison.OrdinalIgnoreCase) ||
            tag.StartsWith(language + "-", StringComparison.OrdinalIgnoreCase);
    }

    public static SpeechVoice[] ForLanguage(IEnumerable<SpeechVoice> voices, string language) =>
        voices.Where(voice => MatchesLanguage(voice.Language, language))
            .DistinctBy(voice => voice.Id, StringComparer.Ordinal)
            .OrderBy(voice => voice.Name, StringComparer.CurrentCultureIgnoreCase)
            .ThenBy(voice => voice.Language, StringComparer.OrdinalIgnoreCase)
            .ThenBy(voice => voice.Id, StringComparer.Ordinal)
            .ToArray();

    public static string? ResolveId(IEnumerable<SpeechVoice> voices, string language,
        string? preferredId, string? systemDefaultId)
    {
        var candidates = ForLanguage(voices, language);
        return candidates.FirstOrDefault(voice => voice.Id == preferredId)?.Id
            ?? candidates.FirstOrDefault(voice => voice.Id == systemDefaultId)?.Id
            ?? candidates.FirstOrDefault()?.Id;
    }
}
