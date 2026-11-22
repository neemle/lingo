using System.Text.Json.Serialization;

namespace Reader.Contracts;

public sealed record ReaderPreferences
{
    public string? Layout { get; init; }
    public bool? AutoTranslate { get; init; }
    public bool? ContinuousSpeech { get; init; }
    public int? SpeechRatePercent { get; init; }
    public Dictionary<string, string?> TranslationTargets { get; init; } = new(StringComparer.Ordinal);
    public Dictionary<string, string?> SpeechVoices { get; init; } = new(StringComparer.Ordinal);
}

public sealed record ReaderPreferenceChange(string Field)
{
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public object? Value { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? RatePercent { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? Continuous { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Source { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Target { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Language { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? VoiceId { get; private init; }

    public static ReaderPreferenceChange Layout(string value) => new("layout") { Value = value };
    public static ReaderPreferenceChange AutoTranslate(bool value) => new("autoTranslate") { Value = value };
    public static ReaderPreferenceChange SpeechPlayback(int ratePercent, bool continuous) =>
        new("speechPlayback") { RatePercent = ratePercent, Continuous = continuous };
    public static ReaderPreferenceChange TranslationTarget(string source, string target) =>
        new("translationTarget") { Source = source, Target = target };
    public static ReaderPreferenceChange SpeechVoice(string language, string? voiceId) =>
        new("speechVoice") { Language = language, VoiceId = voiceId };
}
