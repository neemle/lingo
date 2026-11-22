namespace Reader.Contracts;

public sealed record TranslationPair(string Source, string Target);

public sealed record TranslationReadiness(bool Ready, string? PivotLanguage, TranslationPair[] Missing);

public sealed record TranslationPackageStatus(
    string Source, string Target, bool FilesPresent, string Revision, TranslationAsset[] Assets,
    bool HasLocalFiles = false, string? Repository = null, string? License = null);

public sealed record TranslationAsset(string Name, long Bytes, string Sha256);
