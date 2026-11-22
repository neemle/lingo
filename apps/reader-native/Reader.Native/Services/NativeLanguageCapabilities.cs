using Reader.Presentation;

namespace Reader.Native.Services;

internal sealed record NativeLanguageCapabilities(
    NativeRecognitionInventory Recognition, SpeechVoice[] Voices, string? VoiceError)
{
    internal static NativeLanguageCapabilities Read()
    {
        NativeRecognitionInventory recognition;
        try { recognition = ReadRecognition(); }
        catch (Exception error)
        {
            recognition = new(CapabilityLevel.Unavailable, [], $"Не вдалося перевірити OCR. {error.Message}");
        }
        try { return new(recognition, NativeSpeech.InstalledVoices(), null); }
        catch (Exception error)
        {
            return new(recognition, [], $"Не вдалося перелічити системні голоси. {error.Message}");
        }
    }

#if WINDOWS
    private static NativeRecognitionInventory ReadRecognition()
    {
        if (!PhotoOcr.HasPackageIdentity())
            return new(CapabilityLevel.Unavailable, [], PhotoOcr.PackageRequired, "Windows.Media.Ocr");
        var tags = PhotoOcr.AvailableRecognitionLanguages().Select(item => item.LanguageTag).ToArray();
        return new(CapabilityLevel.Installed, tags, "Windows не повідомила про встановлений OCR цієї мови.",
            "Windows.Media.Ocr");
    }
#elif MACCATALYST
    private static NativeRecognitionInventory ReadRecognition() => new(CapabilityLevel.Available,
        PhotoOcr.RecognitionLanguageTags(), "Apple Vision не перелічує цю мову для поточного режиму OCR.", "Apple Vision");
#endif
}
