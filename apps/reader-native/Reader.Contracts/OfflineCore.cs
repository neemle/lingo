using System.Text.Json;
using System.Text.Json.Serialization;

namespace Reader.Contracts;

public sealed record DocumentBlock(string Kind, int StartUtf8, int EndUtf8,
    int StartUtf16, int EndUtf16, int? Page, int? HeadingLevel)
{
    public bool Footnote { get; init; }
    public OcrSpan[] FootnoteReferences { get; init; } = [];
    public DocxList? List { get; init; }
    public DocxTableCell? TableCell { get; init; }
}

public sealed record DocxList(uint Id, int Level)
{
    public string? Marker { get; init; }
}
public sealed record DocxTableCell(uint Table, uint Row, uint Column, uint ColumnSpan);

public sealed record CachedTranslation(ParagraphTranslation Paragraph, MachineTranslation? Sentence);
public sealed record ParagraphTranslation(ParagraphUnit[] Units, string? PivotLanguage,
    bool MoreBefore, bool MoreAfter);
public sealed record ParagraphUnit(string[] SentenceIds, string? Translation, bool Reviewed, string? PivotLanguage = null,
    string? DirectModelRevision = null, TranslationAlignment? Alignment = null);

public sealed record LexicalPackage(string Language, string Name, string SourceUrl, string DownloadUrl,
    string Revision, string License, string GlossLanguage, long Bytes, string Sha256, bool Installed);
public sealed record MeaningExample(string Text, string? Translation);
public sealed record WordMeaning(string Id, string Lemma, string PartOfSpeech, string Gloss,
    string GlossLanguage, MeaningExample[] Examples, string[] Tags, string SourceName, string SourceUrl,
    string SourceLicense, string SourceRevision, string Confidence, int ContextMatches);

public sealed record DictionaryArchive(int Version, Vocabulary[] Entries, DictionaryReview[] Reviews,
    DictionarySource[] Sources,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] PracticeProgress? Practice = null);
public sealed record DictionaryReview(string Id, string EntryId, string Direction, byte Grade, long ReviewedAt,
    string? Answer = null, long? ElapsedMs = null);
public sealed record DictionarySource(string DocumentId, string? Title, string? SourceHash, string[] EntryIds,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] DictionaryExample[]? Examples = null);

public sealed record DictionaryExample(string EntryId, string Surface, string Sentence,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] DictionarySentenceAnchor[]? Anchors = null);
public sealed record DictionarySourceSelection(int StartUtf16, string Text);
public sealed record DictionarySentenceAnchor(string SentenceId, string RevisionId, int? Page, int SentenceStartUtf16,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] DictionarySourceSelection? Selection = null);
public sealed record DictionarySourceLocation(string DocumentId, string Title, string Sentence, string SentenceId,
    string RevisionId, int? Page, int Position, DictionarySourceSelection? Selection = null);
public sealed record DictionaryImport(int AddedCards, int ExistingCards, int AddedReviews, bool RestoredPractice = false);

public sealed record SpeechAudio(int SampleRate, int Samples,
    [property: JsonConverter(typeof(SpeechWavConverter))] byte[] WavBytes)
{
    public float PlaybackRate { get; init; } = 1;
}

internal sealed class SpeechWavConverter : JsonConverter<byte[]>
{
    public SpeechWavConverter() { }

    public override byte[] Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType == JsonTokenType.String)
            return reader.GetBytesFromBase64();
        if (reader.TokenType != JsonTokenType.StartArray)
            throw new JsonException("Invalid WAV byte sequence.");
        var bytes = new List<byte>();
        while (reader.Read() && reader.TokenType != JsonTokenType.EndArray)
        {
            if (reader.TokenType != JsonTokenType.Number || !reader.TryGetByte(out var value))
                throw new JsonException("Invalid WAV byte.");
            bytes.Add(value);
        }
        if (reader.TokenType != JsonTokenType.EndArray)
            throw new JsonException("Incomplete WAV byte sequence.");
        return bytes.ToArray();
    }

    public override void Write(Utf8JsonWriter writer, byte[] value, JsonSerializerOptions options) =>
        writer.WriteBase64StringValue(value);
}
