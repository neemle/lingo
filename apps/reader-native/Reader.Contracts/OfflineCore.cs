using System.Text.Json.Serialization;

namespace Reader.Contracts;

public sealed record DocumentBlock(string Kind, int StartUtf8, int EndUtf8,
    int StartUtf16, int EndUtf16, int? Page, int? HeadingLevel)
{
    public DocxList? List { get; init; }
    public DocxTableCell? TableCell { get; init; }
}

public sealed record DocxList(uint Id, int Level);
public sealed record DocxTableCell(uint Table, uint Row, uint Column, uint ColumnSpan);

public sealed record CachedTranslation(ParagraphTranslation Paragraph, MachineTranslation? Sentence);
public sealed record ParagraphTranslation(ParagraphUnit[] Units, string? PivotLanguage,
    bool MoreBefore, bool MoreAfter);
public sealed record ParagraphUnit(string[] SentenceIds, string? Translation, bool Reviewed);

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
public sealed record DictionarySentenceAnchor(string SentenceId, string RevisionId, int? Page, int SentenceStartUtf16);
public sealed record DictionarySourceLocation(string DocumentId, string Title, string Sentence, string SentenceId,
    string RevisionId, int? Page, int Position);
public sealed record DictionaryImport(int AddedCards, int ExistingCards, int AddedReviews, bool RestoredPractice = false);

public sealed record SpeechAudio(int SampleRate, int Samples, int[] WavBytes);
