namespace Reader.Contracts;

public sealed record KnowledgePackage(string Language, string Name, string Version, string SourceUrl,
    string DownloadUrl, string Revision, string License, long Bytes, string Sha256, bool Installed,
    bool ContextAvailable, bool ExpressionsAvailable);

public sealed record WordAnalysis(string Lemma, string PartOfSpeech, string Features, int Observations,
    int ContextObservations);

public sealed record WordContext(string Sentence, int StartUtf16);

public sealed record ExpressionMatch(string Text, string Canonical, string Kind, int Observations);

public sealed record WordLookup(KnowledgePackage Package, int TotalObservations, WordAnalysis[] Analyses,
    ExpressionMatch[] Expressions);
