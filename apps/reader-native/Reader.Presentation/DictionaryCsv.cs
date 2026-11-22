using System.Text;
using Microsoft.VisualBasic.FileIO;
using Reader.Contracts;

namespace Reader.Presentation;

public static class DictionaryCsv
{
    public const int MaxBytes = 16 * 1024 * 1024;
    private static readonly string[] Columns = ["word", "translation", "source_language", "target_language"];
    private static readonly UTF8Encoding Utf8 = new(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true);

    public static byte[] Encode(IEnumerable<Vocabulary> cards)
    {
        var csv = new StringBuilder("\uFEFF").AppendJoin(',', Columns).Append("\r\n");
        foreach (var card in cards.DistinctBy(Key))
        {
            csv.AppendJoin(',', Quote(card.Word), Quote(card.Translation), Quote(card.Language), Quote(card.TargetLanguage))
                .Append("\r\n");
            if (csv.Length > MaxBytes) throw new InvalidDataException("CSV словника перевищує 16 МіБ.");
        }
        var content = csv.ToString();
        if (Utf8.GetByteCount(content) > MaxBytes)
            throw new InvalidDataException("CSV словника перевищує 16 МіБ.");
        return Utf8.GetBytes(content);
    }

    public static DictionaryArchive Decode(byte[] bytes)
    {
        if (bytes.Length > MaxBytes) throw new InvalidDataException("CSV словника перевищує 16 МіБ.");
        string content;
        try { content = Utf8.GetString(bytes).TrimStart('\uFEFF'); }
        catch (DecoderFallbackException error)
        {
            throw new InvalidDataException("CSV має бути в кодуванні UTF-8.", error);
        }
        var endOfHeader = content.IndexOfAny(['\r', '\n']);
        var headerLine = endOfHeader >= 0 ? content.AsSpan(0, endOfHeader) : content.AsSpan();
        using var parser = new TextFieldParser(new StringReader(content))
        {
            HasFieldsEnclosedInQuotes = true,
            TrimWhiteSpace = false,
        };
        parser.SetDelimiters(headerLine.Contains(';') ? ";" : ",");
        try
        {
            if (parser.ReadFields() is not { } header || !header.SequenceEqual(Columns, StringComparer.OrdinalIgnoreCase))
                throw new InvalidDataException("Колонки CSV мають бути word, translation, source_language, target_language.");
            var cards = new Dictionary<string, Vocabulary>(StringComparer.Ordinal);
            while (!parser.EndOfData)
            {
                var line = parser.LineNumber;
                if (parser.ReadFields() is not [var word, var translation, var source, var target])
                    throw new InvalidDataException($"У рядку {line} CSV має бути чотири колонки.");
                word = Unescape(word);
                translation = Unescape(translation);
                source = source.Trim().ToLowerInvariant();
                target = target.Trim().ToLowerInvariant();
                if (string.IsNullOrWhiteSpace(word) || Utf8.GetByteCount(word) > 500 ||
                    string.IsNullOrWhiteSpace(translation) || Utf8.GetByteCount(translation) > 2000 ||
                    !DocumentLanguages.All.Any(language => language.Code == source) ||
                    !DocumentLanguages.All.Any(language => language.Code == target))
                    throw new InvalidDataException($"Некоректна картка в рядку {line} CSV.");
                var card = new Vocabulary(Guid.NewGuid().ToString("N"), word, translation, source, target, "", "",
                    new ReviewState(), new ReviewState());
                cards.TryAdd(Key(card), card);
            }
            return new DictionaryArchive(1, cards.Values.ToArray(), [], []);
        }
        catch (MalformedLineException error)
        {
            throw new InvalidDataException($"Некоректні лапки в рядку {error.LineNumber} CSV.", error);
        }
    }

    private static string Key(Vocabulary card) =>
        $"{card.Language}|{card.TargetLanguage}|{card.Word.ToLowerInvariant()}|{card.Translation.ToLowerInvariant()}";

    private static string Quote(string value)
    {
        if (NeedsSpreadsheetEscape(value)) value = "'" + value;
        return "\"" + value.Replace("\"", "\"\"") + "\"";
    }

    private static string Unescape(string value) =>
        value.Length > 1 && value[0] == '\'' && NeedsSpreadsheetEscape(value[1..]) ? value[1..] : value;

    private static bool NeedsSpreadsheetEscape(string value)
    {
        if (value.Length == 0) return false;
        var visible = value.AsSpan().TrimStart();
        return char.IsControl(value[0]) || (!visible.IsEmpty &&
            visible[0] is '=' or '+' or '-' or '@' or '\'' or '＝' or '＋' or '－' or '＠');
    }
}
