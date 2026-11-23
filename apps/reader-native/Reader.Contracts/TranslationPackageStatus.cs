namespace Reader.Contracts;

public sealed record TranslationPair(string Source, string Target);

public sealed record TranslationReadiness(bool Ready, string? PivotLanguage, TranslationPair[] Missing,
    TranslationPair? DirectUpgrade = null);

public sealed record TranslationRouteStatus(string Source, string Target, bool Ready, string? PivotLanguage);

public sealed record TranslationPackageStatus(
    string Source, string Target, bool FilesPresent, string Revision, TranslationAsset[] Assets,
    bool HasLocalFiles = false, string? Repository = null, string? License = null,
    TranslationPackageValidation? Validation = null)
{
    public bool HasRecoveryCopy { get; init; }
    public bool HasValidatedTranslation => FilesPresent && Validation?.Matches(this) == true;
}

public sealed record TranslationPackageValidation(
    string Source, string Target, string Repository, string Revision, string Input, string Output)
{
    public bool Matches(TranslationPackageStatus package) =>
        Source == package.Source && Target == package.Target &&
        Repository == package.Repository && Revision == package.Revision;
}

public sealed record TranslationAsset(string Name, long Bytes, string Sha256);
