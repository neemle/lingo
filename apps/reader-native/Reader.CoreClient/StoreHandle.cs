using Microsoft.Win32.SafeHandles;

namespace Reader.CoreClient;

internal sealed class StoreHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal StoreHandle(ulong value) : base(true) => SetHandle(checked((nint)value));
    internal ulong Value => checked((ulong)DangerousGetHandle());
    protected override bool ReleaseHandle() => NativeMethods.reader_close(Value) == 0;
}
