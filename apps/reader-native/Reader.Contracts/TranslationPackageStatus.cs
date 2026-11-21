namespace Reader.Contracts;

public sealed record TranslationPackageStatus(string Source, string Target, bool FilesPresent);
