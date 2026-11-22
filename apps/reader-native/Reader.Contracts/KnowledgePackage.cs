namespace Reader.Contracts;

public sealed record KnowledgePackage(string Language, string Name, string Version, string SourceUrl,
    string DownloadUrl, string Revision, string License, long Bytes, string Sha256, bool Installed);

public sealed record WordAnalysis(string Lemma, string PartOfSpeech, string Features, int Observations);

public sealed record WordLookup(KnowledgePackage Package, int TotalObservations, WordAnalysis[] Analyses);
