using Reader.Contracts;

namespace Reader.CoreClient;

public sealed class CoreException(BridgeError error) : Exception(error.Message)
{
    public BridgeError Error { get; } = error;
}
