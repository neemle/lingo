using System.Text.Json.Serialization;

namespace Reader.Contracts;

public sealed record ReaderPreferences
{
    public string? Layout { get; init; }
    public bool? PdfShowOriginal { get; init; }
    public bool? PdfFitWidth { get; init; }
    public string? PdfFit { get; init; }
    public bool? PdfTranslationLayout { get; init; }
    public bool? AutoTranslate { get; init; }
    public bool? BackgroundTranslation { get; init; }
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
    public string? Source { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Target { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Language { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? VoiceId { get; private init; }

    public static ReaderPreferenceChange Layout(string value) => new("layout") { Value = value };
    public static ReaderPreferenceChange PdfShowOriginal(bool value) => new("pdfShowOriginal") { Value = value };
    public static ReaderPreferenceChange PdfFit(string value) => new("pdfFit") { Value = value };
    public static ReaderPreferenceChange PdfTranslationLayout(bool value) => new("pdfTranslationLayout") { Value = value };
    public static ReaderPreferenceChange AutoTranslate(bool value) => new("autoTranslate") { Value = value };
    public static ReaderPreferenceChange BackgroundTranslation(bool value) => new("backgroundTranslation") { Value = value };
    public static ReaderPreferenceChange SpeechRate(int ratePercent) => new("speechRatePercent") { Value = ratePercent };
    public static ReaderPreferenceChange ContinuousSpeech(bool value) => new("continuousSpeech") { Value = value };
    public static ReaderPreferenceChange TranslationTarget(string source, string target) =>
        new("translationTarget") { Source = source, Target = target };
    public static ReaderPreferenceChange SpeechVoice(string language, string? voiceId) =>
        new("speechVoice") { Language = language, VoiceId = voiceId };
}
