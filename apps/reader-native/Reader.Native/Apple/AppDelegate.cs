using Foundation;
using Reader.Native.Services;
using UIKit;

namespace Reader.Native;

[Register("AppDelegate")]
public class AppDelegate : MauiUIApplicationDelegate
{
    public override bool OpenUrl(UIApplication app, NSUrl url, NSDictionary options) =>
        DesktopFiles.QueueFiles([url]) || base.OpenUrl(app, url, options);

#if MACCATALYST
    public override void BuildMenu(IUIMenuBuilder builder)
    {
        base.BuildMenu(builder);
        if (builder.System != UIMenuSystem.MainSystem) return;

        var command = UIKeyCommand.Create("Додати документ…", null, new ObjCRuntime.Selector("lingoOpenDocuments:"),
            "o", UIKeyModifierFlags.Command, null);
        var open = UIMenuIdentifier.Open.GetConstant()!.ToString();
        if (builder.GetMenu(open) is not null)
            builder.ReplaceChildrenOfMenu(open, children => [command, .. children.Where(child => child is not UICommand)]);
        else
            builder.InsertElementsAtStartOfMenu([command], UIMenuIdentifier.File.GetConstant()!.ToString());
    }

    [Export("lingoOpenDocuments:")]
    public async void OpenDocuments(UIKeyCommand command)
    {
        if (Shell.Current is AppShell shell) await shell.PickDocumentsAsync();
    }
#endif

	protected override MauiApp CreateMauiApp() => MauiProgram.CreateMauiApp();
}
