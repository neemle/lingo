using System.ComponentModel;
using Reader.Native.Features.Library;
using Reader.Native.Features.Reading;
using Reader.Native.Services;
using Reader.Presentation;

namespace Reader.Native;

public partial class AppShell : Shell
{
    internal const string LibraryRoute = "//workspace/librarySection/library";
    internal const string ReadingRoute = "//workspace/readingSection/reading";
    internal const string PracticeRoute = "//workspace/practiceSection/practice";
    internal const string SettingsRoute = "//workspace/settingsSection/settings";
    private static readonly string[] Sections = ["library", "reading", "vocabulary", "practice", "settings"];
    private const string OriginalPdfPreference = "lastOriginalPdf";
    private readonly string previousRoute = RestoreRoute();
    private readonly string previousOriginalPdf = Preferences.Get(OriginalPdfPreference, "");
    private ReaderSession? trackedSession;
    private string[] renderedRecent = [];
    private bool restored;
    private bool openingFiles;
    private bool listening;
    private bool stopped;

    public AppShell()
	{
		InitializeComponent();
	}

    internal async void RestoreAfterActivation()
    {
        if (restored || stopped) return;
        restored = true;
        try
        {
            trackedSession = AppState.Session;
            var id = Preferences.Get("lastDocument", "");
            if (id.Length > 0) await trackedSession.SelectAsync(id);
            else await trackedSession.RefreshAsync();
            if (!stopped) AppState.StartTranslationWorker(Dispatcher);
            if (!stopped && !DesktopImport.HasIncoming && previousRoute != LibraryRoute)
            {
                await GoToAsync(previousRoute);
                if (!stopped && previousOriginalPdf == id && trackedSession.Document is { Format: "pdf", Id: var opened }
                    && opened == id && CurrentPage is ReadingPage reader)
                    await reader.ShowOriginalAsync();
            }
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error);
            if (!stopped) await DisplayAlertAsync("Локальний читач", error.Message, "Гаразд");
        }
        finally
        {
            if (!stopped)
            {
                listening = true;
                if (trackedSession is not null) trackedSession.PropertyChanged += SessionChanged;
                RefreshRecent();
                DesktopImport.Available += ScheduleFileImports;
                ScheduleFileImports();
            }
        }
    }

    internal void StopListening()
    {
        stopped = true;
        listening = false;
        DesktopImport.Available -= ScheduleFileImports;
        if (trackedSession is not null) trackedSession.PropertyChanged -= SessionChanged;
        trackedSession = null;
    }

    private void ScheduleFileImports() => MainThread.BeginInvokeOnMainThread(DrainFileImports);

    private async void DrainFileImports()
    {
        if (openingFiles || !listening) return;
        openingFiles = true;
        try
        {
            while (listening)
            {
                if (DesktopImport.TakeFailure() is { } failure)
                    await DisplayAlertAsync("Відкрити файл", failure, "Гаразд");
                if (!DesktopImport.HasPending) break;
                await DesktopImport.Gate.WaitAsync();
                try
                {
                    if (!listening) return;
                    if (CurrentPage is not LibraryPage) await GoToAsync(LibraryRoute);
                    if (CurrentPage is not LibraryPage library || !DesktopImport.TryTake(out var items) || items is null)
                        return;
                    await library.ImportExternalBatchAsync(items);
                }
                finally { DesktopImport.Gate.Release(); }
            }
        }
        catch (Exception error) { await DisplayAlertAsync("Відкрити файл", error.Message, "Гаразд"); }
        finally { openingFiles = false; }
    }

    private static string RestoreRoute()
    {
        var last = Preferences.Get("lastRoute", LibraryRoute);
        var queryStart = last.IndexOf('?');
        var path = queryStart >= 0 ? last[..queryStart] : last;
        var query = queryStart >= 0 ? last[queryStart..] : "";
        var route = path.Split('/').LastOrDefault(Sections.Contains) ?? "library";
        return $"//workspace/{route}Section/{route}{query}";
    }

    private void SessionChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(ReaderSession.Snapshot)) RefreshRecent();
    }

    internal void RefreshRecent()
    {
        if (trackedSession is not { } session) return;
        var byId = session.Snapshot.Documents.ToDictionary(document => document.Id);
        var recent = Preferences.Get("recentDocuments", "").Split(';')
            .Prepend(Preferences.Get("lastDocument", ""))
            .Distinct().Where(byId.ContainsKey).Take(3).Select(id => byId[id]).ToArray();
        var key = recent.Select(document => $"{document.Id}:{document.Title}:{document.Language}").ToArray();
        if (renderedRecent.SequenceEqual(key)) return;
        renderedRecent = key;
        RecentHeading.IsVisible = recent.Length > 0;
        RecentBooks.Children.Clear();
        foreach (var document in recent)
        {
            var button = new Button
            {
                Text = document.Title,
                ImageSource = "nav_file.png",
                ContentLayout = new Button.ButtonContentLayout(Button.ButtonContentLayout.ImagePosition.Left, 7),
                TextTransform = TextTransform.None,
                LineBreakMode = LineBreakMode.TailTruncation,
                Padding = new Thickness(7, 4),
                FontSize = 11,
                BackgroundColor = Colors.Transparent,
                BorderWidth = 0,
                MinimumHeightRequest = 34,
            };
            SemanticProperties.SetDescription(button, $"{document.Title}, {DocumentLanguages.Name(document.Language)}");
            button.Clicked += async (_, _) => await AppState.OpenAsync(document.Id);
            RecentBooks.Children.Add(button);
        }
    }

    protected override void OnNavigated(ShellNavigatedEventArgs args)
    {
        base.OnNavigated(args);
        Preferences.Set("lastRoute", args.Current.Location.OriginalString);
        if (CurrentPage is OriginalPage { PdfDocumentId: { } pdf })
            Preferences.Set(OriginalPdfPreference, pdf);
        else
            Preferences.Remove(OriginalPdfPreference);
    }
}
