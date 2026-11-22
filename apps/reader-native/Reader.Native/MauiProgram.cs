using Reader.Native.Controls;

namespace Reader.Native;

public static class MauiProgram
{
    public static MauiApp CreateMauiApp() => MauiApp.CreateBuilder()
        .UseMauiApp<App>()
        .ConfigureMauiHandlers(handlers =>
        {
            handlers.AddHandler<NativeReaderView, NativeReaderHandler>();
#if MACCATALYST
            handlers.AddHandler<Border, FiniteBorderHandler>();
#endif
        })
        .Build();
}
