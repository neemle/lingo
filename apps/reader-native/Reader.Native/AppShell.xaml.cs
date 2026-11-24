using System.ComponentModel;
using Reader.Native.Features.Library;
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
    private readonly string previousRoute = RestoreRoute();
    private ReaderSession? trackedSession;
    private string[] renderedRecent = [];
    private bool restored;
    private bool openingFiles;
    private bool listening;
    private bool stopped;
    internal bool IsClosing => stopped;

    public AppShell()
	{
		InitializeComponent();
#if IOS
        // Custom flyout chrome can loop in UIKit layout before the first page appears.
        FlyoutHeader = null;
        FlyoutFooter = null;
        FlyoutBehavior = FlyoutBehavior.Flyout;
#endif
	}

    internal async void RestoreAfterActivation()
    {
        if (restored || stopped) return;
        restored = true;
        try
        {
            var session = trackedSession = AppState.Session;
            await session.WaitUntilIdleAsync();
            if (stopped) return;
            var id = Preferences.Get("lastDocument", "");
            if (id.Length > 0) await session.SelectAsync(id);
            else await session.RefreshAsync();
            await AppState.RefreshRecentReadingHistoryAsync();
            if (stopped) return;
            await session.WaitUntilIdleAsync();
            if (stopped) return;
            if (session.Document is null && !DesktopImport.HasIncoming &&
                AppState.RecentReadingDocuments(session.Snapshot).FirstOrDefault() is { } recent)
                await session.SelectAsync(recent.Id);
            if (!stopped) AppState.StartTranslationWorker(Dispatcher);
            if (!stopped && !DesktopImport.HasIncoming && previousRoute != LibraryRoute &&
                (!previousRoute.StartsWith(ReadingRoute, StringComparison.Ordinal) || session.Document is not null))
                await GoToAsync(previousRoute);
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
                if (!DesktopImport.HasPending) break;
                await DesktopImport.Gate.WaitAsync();
                try
                {
                    if (!listening || !DesktopImport.TryPeek(out var next) || next is null) return;
                    if (next.Items.Length > 0 && CurrentPage is not LibraryPage) await GoToAsync(LibraryRoute);
                    var library = CurrentPage as LibraryPage;
                    if ((next.Items.Length > 0 && library is null) || !listening ||
                        !DesktopImport.TryTake(out var batch) || batch is null) return;
                    if (batch.Failure is { } failure)
                        await DisplayAlertAsync("Відкрити файл", failure, "Гаразд");
                    if (listening && batch.Items.Length > 0 && library is not null)
                        await library.ImportExternalBatchAsync(batch.Items);
                }
                finally { DesktopImport.Gate.Release(); }
            }
        }
        catch (Exception error)
        {
            if (listening) await DisplayAlertAsync("Відкрити файл", error.Message, "Гаразд");
        }
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
        var recent = AppState.RecentReadingDocuments(session.Snapshot).Take(3).ToArray();
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
    }
}
