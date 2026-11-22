namespace Reader.Contracts;

public sealed record OptionalModel(string Id, string Name, string License, string Revision,
    bool FilesPresent, bool HasLocalFiles, OptionalModelAsset[] Assets);

public sealed record OptionalModelAsset(string Name, string DownloadUrl, long Bytes, string Sha256);
