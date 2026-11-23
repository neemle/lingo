using System.Globalization;
using System.Text;
using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Reading;

public partial class ReadingPage
{
    private const string LegacyMigrated = "reader.preferences.migrated";
    private const string LayoutPreference = "reader.layout";
    private const string RatePreference = "reader.speech.rate";
    private const string ContinuousPreference = "reader.speech.continuous";
    private const string AutoTranslatePreference = "reader.translation.follow";
    private const string TargetPreferencePrefix = "reader.translation.target.";
    private const string VoicePreferencePrefix = "reader.speech.voice.";
    private const int DefaultSpeechRate = LocalSpeech.DefaultRatePercent;
    private static readonly int[] LegacySpeechRates = [75, 100, 125];
    private static readonly int[] DefaultSpeechRates = [DefaultSpeechRate, 45, 50, 55, 60, 65, 75, 85, 100, 115, 125];

    private ReaderPreferences readerPreferences = new();
    private int[] speechRates = DefaultSpeechRates;

    private void LoadReaderPreferences()
    {
        try
        {
            var session = AppState.Session;
            var legacy = Preferences.Default;
            if (legacy.Get(LegacyMigrated, false))
                readerPreferences = session.GetReaderPreferences();
            else
            {
                var targets = new Dictionary<string, string?>(StringComparer.Ordinal);
                var voices = new Dictionary<string, string?>(StringComparer.Ordinal);
                foreach (var language in DocumentLanguages.All)
                {
                    var source = language.Code;
                    var target = legacy.Get(TargetPreferencePrefix + source, "");
                    if (target != source && DocumentLanguages.All.Any(entry => entry.Code == target))
                        targets[source] = target;
                    var voiceKey = VoicePreferencePrefix + source;
                    if (legacy.ContainsKey(voiceKey))
                    {
                        var voice = legacy.Get(voiceKey, "");
                        if (voice.Length == 0)
                            voices[source] = null;
                        else if (!string.IsNullOrWhiteSpace(voice) && !voice.Any(char.IsControl) &&
                            Encoding.UTF8.GetByteCount(voice) <= 512)
                            voices[source] = voice;
                    }
                }
                var layout = (ReaderLayout)legacy.Get(LayoutPreference, -1);
                var rate = legacy.Get(RatePreference, -1);
                readerPreferences = session.MergeReaderPreferences(new ReaderPreferences
                {
                    Layout = Enum.IsDefined(layout) ? LayoutName(layout) : null,
                    AutoTranslate = legacy.ContainsKey(AutoTranslatePreference) ? legacy.Get(AutoTranslatePreference, false) : null,
                    ContinuousSpeech = legacy.ContainsKey(ContinuousPreference) ? legacy.Get(ContinuousPreference, false) : null,
                    SpeechRatePercent = rate >= 0 && rate < LegacySpeechRates.Length ? LegacySpeechRates[rate] : null,
                    TranslationTargets = targets,
                    SpeechVoices = voices,
                });
                legacy.Set(LegacyMigrated, true);
            }
        }
        catch (Exception error) { ReportPreferenceError(error); }
    }

    private static string LayoutName(ReaderLayout layout) => layout.ToString().ToLowerInvariant();

    private void SaveReaderPreference(ReaderPreferenceChange change)
    {
        try
        {
            readerPreferences = AppState.Session.UpdateReaderPreference(change);
        }
        catch (Exception error) { ReportPreferenceError(error); }
    }

    private void ReportPreferenceError(Exception error) =>
        _ = DisplayAlertAsync("Налаштування читача", error.Message, "Гаразд");

    private void ApplySpeechPreferences()
    {
        var rate = readerPreferences.SpeechRatePercent ?? DefaultSpeechRate;
        speechRates = DefaultSpeechRates.Contains(rate)
            ? DefaultSpeechRates : [.. DefaultSpeechRates.Append(rate).Order()];
        Rate.ItemsSource = DetachedRate.ItemsSource = speechRates.Select(value =>
            (value / 100d).ToString("0.##", CultureInfo.InvariantCulture) + "×").ToArray();
        Rate.SelectedIndex = DetachedRate.SelectedIndex = Array.IndexOf(speechRates, rate);
        ContinueSpeech.IsChecked = DetachedContinueSpeech.IsChecked = readerPreferences.ContinuousSpeech ?? false;
    }
}
