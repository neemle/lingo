namespace Reader.Presentation;

public static class UkrainianStressEdit
{
    private const string AcuteAccent = "\u0301";

    public static (string Text, int Caret)? AtCaret(string text, int caret)
    {
        if (caret <= 0 || caret > text.Length) return null;

        var ranges = new SourceRanges(text);
        var vowel = ranges.Selection(caret - 1, 1);
        if (vowel is null || !"аеєиіїоуюяАЕЄИІЇОУЮЯ".Contains(vowel.Text[0]))
            return null;
        // The decomposed form of й starts with и, but it is not a vowel.
        if ((vowel.Text[0] is 'и' or 'И') && vowel.Text.Contains('\u0306'))
            return null;
        if (ranges.WordAt(vowel.StartUtf16) is not { } word) return null;

        var vowelEnd = vowel.StartUtf16 + vowel.LengthUtf16;
        var wordEnd = word.StartUtf16 + word.LengthUtf16;
        var stressedPrefix = text[word.StartUtf16..vowelEnd].Replace(AcuteAccent, "") + AcuteAccent;
        var restOfWord = text[vowelEnd..wordEnd].Replace(AcuteAccent, "");
        var updated = string.Concat(text.AsSpan(0, word.StartUtf16), stressedPrefix, restOfWord, text.AsSpan(wordEnd));
        return (updated, word.StartUtf16 + stressedPrefix.Length);
    }
}
