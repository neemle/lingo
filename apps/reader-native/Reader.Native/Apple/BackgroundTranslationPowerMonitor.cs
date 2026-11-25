using Foundation;

namespace Reader.Native.Services;

internal sealed class BackgroundTranslationPowerMonitor : IDisposable
{
    private readonly NSObject powerObserver;
    private readonly NSObject thermalObserver;

    internal BackgroundTranslationPowerMonitor(Action changed)
    {
        // Foundation requires a thermal-state read before registering for changes.
        _ = NSProcessInfo.ProcessInfo.ThermalState;
        thermalObserver = NSProcessInfo.Notifications.ObserveThermalStateDidChange((_, _) => changed());
        powerObserver = NSProcessInfo.Notifications.ObservePowerStateDidChange((_, _) => changed());
    }

    internal static string? PauseReason
    {
        get
        {
            var process = NSProcessInfo.ProcessInfo;
            if (process.ThermalState is NSProcessInfoThermalState.Serious or NSProcessInfoThermalState.Critical)
                return "Фоновий переклад призупинено, поки пристрій охолоне.";
            return process.LowPowerModeEnabled ? "Фоновий переклад призупинено в режимі енергозбереження." : null;
        }
    }

    public void Dispose()
    {
        powerObserver.Dispose();
        thermalObserver.Dispose();
    }
}
