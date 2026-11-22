using Foundation;
using Reader.Native.Services;
using UIKit;

namespace Reader.Native;

[Register("AppDelegate")]
public class AppDelegate : MauiUIApplicationDelegate
{
    public override bool OpenUrl(UIApplication app, NSUrl url, NSDictionary options) =>
        DesktopFiles.QueueFiles([url]) || base.OpenUrl(app, url, options);

	protected override MauiApp CreateMauiApp() => MauiProgram.CreateMauiApp();
}
