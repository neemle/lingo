using System.Reflection;
using System.Runtime.InteropServices;

namespace Reader.CoreClient;

internal static class NativeMethods
{
    private const string Library = "reader_ffi";

    static NativeMethods()
    {
        if (!OperatingSystem.IsMacCatalyst())
            NativeLibrary.SetDllImportResolver(typeof(NativeMethods).Assembly, Resolve);
    }

    private static nint Resolve(string name, Assembly assembly, DllImportSearchPath? searchPath)
    {
        if (name != Library) return 0;
        var filename = OperatingSystem.IsWindows() ? "reader_ffi.dll" :
            OperatingSystem.IsMacOS() ? "libreader_ffi.dylib" : "libreader_ffi.so";
        var path = Path.Combine(AppContext.BaseDirectory, filename);
        if (!File.Exists(path)) throw new DllNotFoundException($"Missing app-local Rust bridge: {path}");
        return NativeLibrary.Load(path);
    }

    internal static uint reader_abi_version() => OperatingSystem.IsMacCatalyst()
        ? CatalystMethods.reader_abi_version() : DesktopVersion();

    internal static NativeBuffer reader_call(ulong handle, byte[] input, ulong length) =>
        OperatingSystem.IsMacCatalyst() ? CatalystMethods.reader_call(handle, input, length)
            : DesktopCall(handle, input, length);

    internal static uint reader_close(ulong handle) => OperatingSystem.IsMacCatalyst()
        ? CatalystMethods.reader_close(handle) : DesktopClose(handle);

    internal static NativeBuffer FitPdfText(byte[] text, ulong length, float width, float height,
        float preferred, float minimum, nint context, nint measure) => OperatingSystem.IsMacCatalyst()
        ? CatalystMethods.reader_fit_pdf_text(text, length, width, height, preferred, minimum, context, measure)
        : DesktopFitPdfText(text, length, width, height, preferred, minimum, context, measure);

    internal static uint reader_release(ulong allocation) => OperatingSystem.IsMacCatalyst()
        ? CatalystMethods.reader_release(allocation) : DesktopRelease(allocation);

    internal static NativeBuffer RotateOcrPng(byte[] input, ulong length, ushort angle) =>
        OperatingSystem.IsMacCatalyst() ? CatalystMethods.reader_rotate_ocr_png(input, length, angle)
            : DesktopRotateOcrPng(input, length, angle);

    [DllImport(Library, EntryPoint = "reader_abi_version", CallingConvention = CallingConvention.Cdecl)]
    private static extern uint DesktopVersion();
    [DllImport(Library, EntryPoint = "reader_call", CallingConvention = CallingConvention.Cdecl)]
    private static extern NativeBuffer DesktopCall(ulong handle, byte[] input, ulong length);
    [DllImport(Library, EntryPoint = "reader_fit_pdf_text", CallingConvention = CallingConvention.Cdecl)]
    private static extern NativeBuffer DesktopFitPdfText(byte[] input, ulong length, float width, float height,
        float preferred, float minimum, nint context, nint measure);
    [DllImport(Library, EntryPoint = "reader_close", CallingConvention = CallingConvention.Cdecl)]
    private static extern uint DesktopClose(ulong handle);
    [DllImport(Library, EntryPoint = "reader_release", CallingConvention = CallingConvention.Cdecl)]
    private static extern uint DesktopRelease(ulong allocation);
    [DllImport(Library, EntryPoint = "reader_rotate_ocr_png", CallingConvention = CallingConvention.Cdecl)]
    private static extern NativeBuffer DesktopRotateOcrPng(byte[] input, ulong length, ushort angle);
}
