namespace Reader.Contracts;

public sealed record OptionalModel(string Id, string Name, string License, string Revision,
    bool FilesPresent, bool HasLocalFiles, OptionalModelAsset[] Assets, ModelMemory? Memory = null);

public sealed record OptionalModelAsset(string Name, string DownloadUrl, long Bytes, string Sha256);

public sealed record ModelMemory(bool Ready, long? AvailableMib, long RequiredMib, long? TotalMib, long MinimumTotalMib);
