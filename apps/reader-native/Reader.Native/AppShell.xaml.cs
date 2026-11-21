namespace Reader.Native;

public partial class AppShell : Shell
{
    private readonly string previousRoute = Preferences.Get("lastRoute", "//library");
    private bool restored;

    public AppShell()
	{
		InitializeComponent();
	}

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        if (restored) return;
        restored = true;
        try
        {
            var id = Preferences.Get("lastDocument", "");
            if (id.Length > 0) await Services.AppState.Session.SelectAsync(id);
            await GoToAsync(previousRoute);
        }
        catch (Exception error) { await DisplayAlertAsync("Локальний читач", error.Message, "Гаразд"); }
    }

    protected override void OnNavigated(ShellNavigatedEventArgs args)
    {
        base.OnNavigated(args);
        Preferences.Set("lastRoute", args.Current.Location.OriginalString);
    }
}
