using Foundation;
using Reader.Native.Services;
using UIKit;

namespace Reader.Native;

[Register("SceneDelegate")]
public sealed class SceneDelegate : MauiUISceneDelegate
{
    public override void WillConnect(UIScene scene, UISceneSession session, UISceneConnectionOptions connectionOptions)
    {
        QueueFiles(connectionOptions.UrlContexts);
        base.WillConnect(scene, session, connectionOptions);
    }

    public override bool OpenUrl(UIScene scene, NSSet<UIOpenUrlContext> urlContexts)
    {
        var handled = QueueFiles(urlContexts);
        return base.OpenUrl(scene, urlContexts) || handled;
    }

    private static bool QueueFiles(NSSet<UIOpenUrlContext> urlContexts)
    {
        var handled = false;
        foreach (var context in urlContexts) handled |= DesktopFiles.QueueFile(context.Url);
        return handled;
    }
}
