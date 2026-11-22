using Microsoft.Maui.Graphics;
using Microsoft.Maui.Handlers;

namespace Reader.Native.Controls;

internal sealed class FiniteBorderHandler : BorderHandler
{
    private bool reportedInvalidBounds;

    public FiniteBorderHandler() { }

    public override void PlatformArrange(Rect rect)
    {
        if (double.IsFinite(rect.X) && double.IsFinite(rect.Y) &&
            double.IsFinite(rect.Width) && double.IsFinite(rect.Height) &&
            double.IsFinite(rect.Center.X) && double.IsFinite(rect.Center.Y))
        {
            base.PlatformArrange(rect);
            return;
        }

        // A later layout pass can arrange this border once measured bounds are available.
        if (reportedInvalidBounds) return;
        reportedInvalidBounds = true;
        var border = VirtualView as VisualElement;
        Console.Error.WriteLine($"Catalyst skipped nonfinite border layout ({border?.AutomationId ?? border?.Parent?.GetType().Name}): {rect}");
    }
}
