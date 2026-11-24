using System.Globalization;
using System.Security.Cryptography;
using System.Text;
using Reader.Contracts;

namespace Reader.Presentation;

public static class SpeechProgress
{
    private const int ModelSynthesisFloorPercent = 80;

    public static string TextHash(Sentence sentence, string? speechText = null) =>
        Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(speechText ?? sentence.Text)));

    public static int? ResumeOffset(Sentence sentence, SpeechResume? resume, string? speechText = null)
    {
        if (resume is null || resume.Model is not null || !Matches(sentence, resume, speechText) ||
            !SupportsWordResume(sentence.Text, speechText ?? sentence.Text))
            return null;
        return Array.BinarySearch(StringInfo.ParseCombiningCharacters(sentence.Text), resume.OffsetUtf16) >= 0
            ? resume.OffsetUtf16 : null;
    }

    public static bool Matches(Sentence sentence, SpeechResume resume, string? speechText = null) =>
        resume.SentenceId == sentence.Id && resume.TextHash == TextHash(sentence, speechText);

    public static bool SupportsWordResume(string source, string speechText)
    {
        if (source == speechText) return true;
        var original = StringInfo.GetTextElementEnumerator(source);
        var spoken = StringInfo.GetTextElementEnumerator(speechText);
        while (original.MoveNext())
        {
            if (!spoken.MoveNext() ||
                WithoutStress(original.GetTextElement()).Normalize() != WithoutStress(spoken.GetTextElement()).Normalize())
                return false;
        }
        return !spoken.MoveNext();

        static string WithoutStress(string element) =>
            element[0] is >= '\u0400' and <= '\u052F' ? element.TrimEnd('\u0301') : element;
    }

    public static bool SameModelTimelineRate(int savedRatePercent, int requestedRatePercent) =>
        // Below the Rust synthesis floor, only the native player's speed changes.
        Math.Max(savedRatePercent, ModelSynthesisFloorPercent) ==
        Math.Max(requestedRatePercent, ModelSynthesisFloorPercent);
}
