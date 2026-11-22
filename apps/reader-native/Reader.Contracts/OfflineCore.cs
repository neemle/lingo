namespace Reader.Contracts;

public sealed record DocumentBlock(string Kind, int StartUtf8, int EndUtf8,
    int StartUtf16, int EndUtf16, int? Page, int? HeadingLevel);

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
    DictionarySource[] Sources);
public sealed record DictionaryReview(string Id, string EntryId, string Direction, byte Grade, long ReviewedAt);
public sealed record DictionarySource(string DocumentId, string? Title, string? SourceHash, string[] EntryIds);
public sealed record DictionaryImport(int AddedCards, int ExistingCards, int AddedReviews);

public sealed record SpeechAudio(int SampleRate, int Samples, int[] WavBytes);
