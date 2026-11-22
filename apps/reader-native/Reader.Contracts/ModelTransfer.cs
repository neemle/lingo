using System.Text.Json.Serialization;

namespace Reader.Contracts;

public sealed record ModelTransferPackage(string Kind)
{
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Id { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Source { get; private init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Target { get; private init; }

    public static ModelTransferPackage Optional(string id) => new("optional") { Id = id };
    public static ModelTransferPackage Translation(string source, string target) =>
        new("translation") { Source = source, Target = target };
}

public sealed record ModelTransferPlan(string Revision, string Status, long ReceivedBytes, long TotalBytes,
    ModelTransferRange? NextRange);

public sealed record ModelTransferRange(string Asset, string Url, long From, long To, long Total);
