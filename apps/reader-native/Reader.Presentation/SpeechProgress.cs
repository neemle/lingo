using System.Globalization;
using System.Security.Cryptography;
using System.Text;
using Reader.Contracts;

namespace Reader.Presentation;

public static class SpeechProgress
{
    public static string TextHash(Sentence sentence, string? speechText = null) =>
        Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(speechText ?? sentence.Text)));

    public static int? ResumeOffset(Sentence sentence, SpeechResume? resume)
    {
        if (resume is null || resume.Model is not null || !Matches(sentence, resume))
            return null;
        return Array.BinarySearch(StringInfo.ParseCombiningCharacters(sentence.Text), resume.OffsetUtf16) >= 0
            ? resume.OffsetUtf16 : null;
    }

    public static bool Matches(Sentence sentence, SpeechResume resume, string? speechText = null) =>
        resume.SentenceId == sentence.Id && resume.TextHash == TextHash(sentence, speechText);
}
