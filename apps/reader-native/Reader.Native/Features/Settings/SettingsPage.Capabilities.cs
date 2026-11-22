using Reader.Contracts;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native.Features.Settings;

public partial class SettingsPage
{
    private TranslationPackageStatus[]? translationInventory;
    private KnowledgePackage[]? knowledgeInventory;
    private bool lexicalInventoryRead;
    private bool optionalInventoryRead;
    private NativeLanguageCapabilities? nativeCapabilities;
    private LanguageCapabilityReport? capabilityReport;

    private void UpdateCapabilityMatrix()
    {
        if (nativeCapabilities is not { } native) return;
        var profiles = LanguageCapabilities.Build(translationInventory, knowledgeInventory,
            lexicalInventoryRead ? lexicalPackages : null,
            optionalInventoryRead ? optionalModels : null,
            native.Voices, native.VoiceError, native.Recognition);
        capabilityReport = new LanguageCapabilityReport(1, DeviceInfo.Platform.ToString(), profiles);
        BindableLayout.SetItemsSource(CapabilityRows, profiles);
        ExportCapabilities.IsEnabled = true;
    }

    private async void ExportCapabilitiesClicked(object? sender, EventArgs args)
    {
        if (capabilityReport is not { } profile) return;
        ExportCapabilities.IsEnabled = false;
        try
        {
            CapabilityStatus.Text = "";
            CapabilityStatus.Text = await NativeFiles.ExportBytesAsync(this, "lingo-language-capabilities.json",
                NativeFiles.Encode(profile)) ?? "";
        }
        catch (Exception error)
        {
            CapabilityStatus.Text = error.Message;
        }
        finally { ExportCapabilities.IsEnabled = true; }
    }
}
