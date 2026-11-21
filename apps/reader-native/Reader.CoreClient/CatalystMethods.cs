using System.Runtime.InteropServices;

namespace Reader.CoreClient;

internal static class CatalystMethods
{
    [DllImport("__Internal", CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint reader_abi_version();
    [DllImport("__Internal", CallingConvention = CallingConvention.Cdecl)]
    internal static extern NativeBuffer reader_call(ulong handle, byte[] input, ulong length);
    [DllImport("__Internal", CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint reader_close(ulong handle);
    [DllImport("__Internal", CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint reader_release(ulong allocation);
}
