using System.Runtime.InteropServices;

namespace Reader.CoreClient;

[StructLayout(LayoutKind.Sequential)]
internal readonly struct NativeBuffer
{
    public readonly nint Data;
    public readonly ulong Length;
    public readonly ulong Allocation;
}
