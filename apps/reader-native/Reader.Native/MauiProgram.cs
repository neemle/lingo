using Reader.Native.Controls;

namespace Reader.Native;

public static class MauiProgram
{
    public static MauiApp CreateMauiApp() => MauiApp.CreateBuilder()
        .UseMauiApp<App>()
        .ConfigureFonts(fonts => fonts.AddFont("fa-solid-900.ttf", "FontAwesomeSolid"))
        .ConfigureMauiHandlers(handlers =>
        {
            handlers.AddHandler<NativeReaderView, NativeReaderHandler>();
#if MACCATALYST
            handlers.AddHandler<Border, FiniteBorderHandler>();
#endif
        })
        .Build();
}
