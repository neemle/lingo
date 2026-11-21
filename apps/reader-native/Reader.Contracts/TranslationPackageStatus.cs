namespace Reader.Contracts;

public sealed record TranslationPackageStatus(
    string Source, string Target, bool FilesPresent, string Revision, TranslationAsset[] Assets);

public sealed record TranslationAsset(string Name, long Bytes, string Sha256);
