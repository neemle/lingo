namespace Reader.Contracts;

public sealed record BridgeError(
    uint Version,
    string Code,
    string Message,
    bool Retryable);
